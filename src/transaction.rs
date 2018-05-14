use std::fmt;
use std::io;
use std::time::{Instant, Duration};

use futures::{Stream, Poll, Async, Future, task};
use futures::future::{self, Either};
use futures::task::Task;
use hyper_tls::HttpsConnector;
use hyper::{self, Request, Client};
use hyper::client::{Response, HttpConnector};
use std::marker::PhantomData;
use tokio_core::reactor::{Handle, Timeout};

use deliverable::Deliverable;
use raii_counter::Counter;

/// The result of the transaction, a message sent to the
/// deliverable.
///
/// This must be sent to the deliverable in any case
/// in order to prevent data loss.
#[derive(Debug)]
pub enum DeliveryResult {
    Dropped,

    Response {
        response: Response,
        body: Vec<u8>,
        duration: Duration,
    },

    Timeout {
        duration: Duration,
    },

    TimeoutError {
        error: io::Error,
        duration: Duration,
    },

    HyperError {
        error: hyper::Error,
        duration: Duration,
    },
}

pub struct Transaction<D: Deliverable> {
    deliverable: D,
    request: Request,
}

impl<D: Deliverable> fmt::Debug for Transaction<D> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Transaction {{ deliverable: (unknown), request: {:?} }}", self.request)
    }
}

struct SpawnedTransaction<D: Deliverable, W: Future, R: Future>
{
    deliverable: Option<D>,
    work: W,
    _counter: Counter,
    start_time: Instant,
    task: Task,

    _r: PhantomData<R>,
}

impl<D: Deliverable, W: Future, R: Future> Drop for SpawnedTransaction<D, W, R> {
    fn drop(&mut self) {
        trace!("Dropping transaction..");
        self.deliverable
            .take()
            .map(|deliverable| {
                deliverable.complete(DeliveryResult::Dropped);
            });
    }
}

impl<D, W, R> Future for SpawnedTransaction<D, W, R>
where
    D: Deliverable,
    W: Future<
        Item=Either<((Response, Vec<u8>), Timeout), ((), R)>,
        Error=Either<(hyper::Error, Timeout), (io::Error, R)>
    >,
    R: Future<
        Item=(Response, Vec<u8>),
        Error=hyper::Error,
    >,
{
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let delivery_result = match self.work.poll() {
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Err(err) => {
                let duration = self.start_time.elapsed();
                // Request errored
                match err {
                    Either::A((hyper_error, _timeout)) => {
                        trace!("Transaction errored during delivery, error: {:?}, duration: {:?}", hyper_error, duration);
                        DeliveryResult::HyperError {
                            error: hyper_error,
                            duration,
                        }
                    },
                    // Timeout errored
                    Either::B((timeout_error, _request)) => {
                        trace!("Transaction errored during timeout, error: {}, duration: {:?}", timeout_error, duration);
                        DeliveryResult::TimeoutError {
                            error: timeout_error,
                            duration,
                        }
                    }
                }
            },
            Ok(Async::Ready(res)) => {
                let duration = self.start_time.elapsed();
                match res {
                    // Got response
                    Either::A(((response, body), _timeout)) => {
                        trace!("Finished transaction with response: {:?}, duration: {:?}", response, duration);
                        DeliveryResult::Response {
                            response,
                            body,
                            duration,
                        }
                    },
                    // Request timed out
                    Either::B((_timeout, _request)) => {
                        trace!("Finished transaction with timeout, duration: {:?}", duration);
                        DeliveryResult::Timeout {
                            duration,
                        }
                    },
                }
            }
        };

        self.deliverable
            .take()
            .map(|deliverable| {
                deliverable.complete(delivery_result);
                self.task.notify();
            });

        Ok(Async::Ready(()))
    }
}

impl<D: Deliverable> Transaction<D> {
    pub fn new(
        deliverable: D,
        request: Request,
    ) -> Transaction<D> {
        Transaction {
            deliverable,
            request,
        }
    }

    pub(crate) fn spawn_request(self, client: &Client<HttpsConnector<HttpConnector>>, handle: &Handle, timeout: Duration, counter: Counter) {
        let Transaction { deliverable, request } = self;
        trace!("Spawning request: {:?}", request);

        let task = task::current();
        let request_future = client.request(request)
            .and_then(|response| {
                let status = response.status();
                let headers = response.headers().clone();
                response.body()
                    .fold(Vec::new(), |mut acc, chunk| {
                        acc.extend_from_slice(&*chunk);
                        future::ok::<_, hyper::Error>(acc)
                    })
                    .map(move |body| {
                        (Response::new().with_status(status).with_headers(headers), body)
                    })
            });

        let start_time = Instant::now();
        match Timeout::new(timeout, handle) {
            Err(error) => {
                deliverable.complete(DeliveryResult::TimeoutError {
                    error,
                    duration: start_time.elapsed(),
                });
                warn!("Could not create timeout on handle for hyper_client_pool::Transaction");
            },
            Ok(timeout) => {
                let timed_request = request_future.select2(timeout);
                handle.spawn(SpawnedTransaction {
                    deliverable: Some(deliverable),
                    work: timed_request,
                    _counter: counter,
                    start_time,
                    task,
                    _r: PhantomData,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate env_logger;

    use hyper_tls::HttpsConnector;
    use hyper;
    use hyper::client::HttpConnector;
    use native_tls::TlsConnector;
    use std::sync::Arc;
    use std::sync::atomic::{Ordering, AtomicUsize};
    use std::thread;
    use tokio_core::reactor::{Core, Timeout};

    use hyper::{Request, Method};
    use super::*;

    #[derive(Debug, Clone)]
    struct DeliveryCounter {
        total_count: Arc<AtomicUsize>,
        response_count: Arc<AtomicUsize>,
    }

    impl DeliveryCounter {
        fn new() -> DeliveryCounter {
            DeliveryCounter {
                total_count: Arc::new(AtomicUsize::new(0)),
                response_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn total_count(&self) -> usize {
            self.total_count.load(Ordering::Acquire)
        }

        fn response_count(&self) -> usize {
            self.response_count.load(Ordering::Acquire)
        }
    }

    impl Deliverable for DeliveryCounter {
        fn complete(self, result: DeliveryResult) {
            if let DeliveryResult::Response { .. } = result {
                self.response_count.fetch_add(1, Ordering::AcqRel);
            }
            self.total_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct SpawnTransactionsFuture {
        client: hyper::Client<HttpsConnector<HttpConnector>>,
        counter: DeliveryCounter,
        handle: Handle,
    }

    const TRANSACTION_SPAWN_COUNT: usize = 200;

    impl Future for SpawnTransactionsFuture {
        type Item = ();
        type Error = ();

        fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
            for _ in 0..TRANSACTION_SPAWN_COUNT {
                let transaction = Transaction::new(self.counter.clone(), Request::new(Method::Get, "https://onesignal.com/".parse().unwrap()));
                transaction.spawn_request(&self.client, &self.handle, Duration::from_secs(10), Counter::new());
            }

            Ok(Async::Ready(()))
        }
    }

    fn test_hyper_client(handle: &Handle) -> hyper::Client<HttpsConnector<HttpConnector>> {
        let tls = TlsConnector::builder().and_then(|builder| builder.build()).unwrap();
        let mut http = HttpConnector::new(4, &handle);
        http.enforce_http(false);
        let connector = HttpsConnector::from((http, tls));
        hyper::Client::configure()
            .connector(connector)
            .build(&handle)
    }

    #[test]
    fn unfinished_transactions_get_sent_to_deliverable() {
        let _ = env_logger::try_init();

        let counter = DeliveryCounter::new();
        let counter_clone = counter.clone();
        let join_handle = thread::spawn(move || {
            let mut core = Core::new().unwrap();
            let handle = core.handle();

            let client = test_hyper_client(&handle);
            let work = SpawnTransactionsFuture {
                client,
                counter: counter_clone,
                handle: handle.clone(),
            }.and_then(|()| {
                Timeout::new(Duration::from_secs(3), &handle).unwrap()
                    .map_err(|_err| {})
            });

            // Run the transactions until the core finishes
            let _ = core.run(work);
        });

        let _ = join_handle.join();

        assert_ne!(counter.response_count(), TRANSACTION_SPAWN_COUNT);
        assert_eq!(counter.total_count(), TRANSACTION_SPAWN_COUNT);
    }

    #[test]
    fn panicked_transactions_get_sent_to_deliverable() {
        let _ = env_logger::try_init();

        let counter = DeliveryCounter::new();
        let counter_clone = counter.clone();
        let join_handle = thread::spawn(move || {
            let mut core = Core::new().unwrap();
            let handle = core.handle();

            let client = test_hyper_client(&handle);
            let work = SpawnTransactionsFuture {
                client,
                counter: counter_clone,
                handle: handle.clone(),
            }.and_then(|()| {
                Timeout::new(Duration::from_secs(3), &handle).unwrap()
                    .map_err(|_err| {
                        panic!("Hahaha, I will panic now.");
                    })
            });

            // Run the transactions until the core finishes
            let _ = core.run(work);
        });

        let _ = join_handle.join();

        assert_ne!(counter.response_count(), TRANSACTION_SPAWN_COUNT);
        assert_eq!(counter.total_count(), TRANSACTION_SPAWN_COUNT);
    }
}
