use std::fmt;
use std::io::{self, ErrorKind};
use std::time::{Duration, Instant};

use futures::future::{self, Either};
use futures::task::Task;
use futures::{task, Async, Future, Poll, Stream};
use hyper::client::connect::Connect;
use hyper::{self, Client, Request};
use hyper::{Body, Response};
use tokio::runtime::current_thread::Handle;
use tokio::timer::{Deadline, DeadlineError};

use deliverable::Deliverable;
use raii_counter::Counter;

/// The result of the transaction, a message sent to the
/// deliverable.
///
/// This must be sent to the deliverable in any case
/// in order to prevent data loss.
#[derive(Debug)]
pub enum DeliveryResult {
    /// The delivery was dropped, unknown if it was sent or not.
    Dropped,

    /// Received a response from the external server.
    Response {
        response: Response<Body>,
        body: Option<Vec<u8>>,
        body_size: usize,
        duration: Duration,
    },

    /// Failed to connect within the timeout limit.
    Timeout { duration: Duration },

    /// The timeout handling had an error.
    TimeoutError {
        error: io::Error,
        duration: Duration,
    },

    /// Sending a request through hyper encountered an error.
    HyperError {
        error: hyper::Error,
        duration: Duration,
    },
}

/// A container type for a [`hyper::Request`] as well as the deliverable
/// which receives the result of the request.
pub struct Transaction<D: Deliverable> {
    deliverable: D,
    request: Request<Body>,
    requires_body: bool,
}

impl<D: Deliverable> fmt::Debug for Transaction<D> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Transaction {{ deliverable: (unknown), request: {:?} }}",
            self.request
        )
    }
}

struct SpawnedTransaction<D: Deliverable, W: Future> {
    deliverable: Option<D>,
    work: W,
    _counter: Counter,
    start_time: Instant,

    /// Because we spawn this future from another task and need
    /// to track its completion as part of ensuring all transactions
    /// are finished, we store a reference to notify the origin task.
    task: Task,
}

impl<D: Deliverable, W: Future> Drop for SpawnedTransaction<D, W> {
    fn drop(&mut self) {
        self.deliverable.take().map(|deliverable| {
            trace!("Dropping transaction..");
            deliverable.complete(DeliveryResult::Dropped);
        });
    }
}

impl<D, W> Future for SpawnedTransaction<D, W>
where
    D: Deliverable,
    W: Future<Item = (Response<Body>, Option<Vec<u8>>, usize), Error = DeadlineError<hyper::Error>>,
{
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let delivery_result = match self.work.poll() {
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Err(deadline_error) => {
                let duration = self.start_time.elapsed();
                if deadline_error.is_timer() {
                    let timer_error = deadline_error.into_timer().expect("is_timer -> into_timer");
                    trace!(
                        "Timer around Transaction errored, error: {:?}, duration: {:?}",
                        timer_error,
                        duration
                    );
                    DeliveryResult::TimeoutError {
                        error: io::Error::new(ErrorKind::Other, timer_error),
                        duration,
                    }
                } else if deadline_error.is_elapsed() {
                    DeliveryResult::Timeout { duration }
                } else if deadline_error.is_inner() {
                    let hyper_error = deadline_error.into_inner().expect("is_inner -> into_inner");
                    trace!(
                        "Transaction errored during delivery, error: {:?}, duration: {:?}",
                        hyper_error,
                        duration
                    );
                    DeliveryResult::HyperError {
                        error: hyper_error,
                        duration,
                    }
                } else {
                    unreachable!("Unexpected deadline_error!");
                }
            }
            Ok(Async::Ready((response, body, body_size))) => {
                let duration = self.start_time.elapsed();
                trace!(
                    "Finished transaction with response: {:?}, duration: {:?}",
                    response,
                    duration
                );
                DeliveryResult::Response {
                    response,
                    body,
                    body_size,
                    duration,
                }
            }
        };

        self.deliverable.take().map(|deliverable| {
            deliverable.complete(delivery_result);
            // Notify the origin task as it might be waiting on the
            // completion of the transaction to finish draining.
            self.task.notify();
        });

        Ok(Async::Ready(()))
    }
}

impl<D: Deliverable> Transaction<D> {
    pub fn new(deliverable: D, request: Request<Body>, requires_body: bool) -> Transaction<D> {
        Transaction {
            deliverable,
            request,
            requires_body,
        }
    }

    pub(crate) fn spawn_request<C: 'static + Connect>(
        self,
        client: &Client<C>,
        handle: &Handle,
        timeout: Duration,
        counter: Counter,
    ) {
        let Transaction {
            deliverable,
            request,
            requires_body,
        } = self;
        trace!("Spawning request: {:?}", request);

        let start_time = Instant::now();
        let task = task::current();
        let request_future = client.request(request).and_then(move |response| {
            if requires_body {
                let (parts, body) = response.into_parts();
                Either::A(
                    body.fold(Vec::new(), |mut acc, chunk| {
                        acc.extend_from_slice(&*chunk);
                        future::ok::<_, hyper::Error>(acc)
                    }).map(move |body| {
                        let body_size = body.len();
                        (
                            Response::from_parts(parts, Body::empty()),
                            Some(body),
                            body_size,
                        )
                    }),
                )
            } else {
                // Note that you must consume the body if you want keepalive
                // to take affect.
                let (parts, body) = response.into_parts();
                Either::B(
                    body.fold(0, |acc, chunk| {
                        future::ok::<_, hyper::Error>(acc + chunk.len())
                    }).map(|body_size| {
                        (Response::from_parts(parts, Body::empty()), None, body_size)
                    }),
                )
            }
        });

        let deadline = Instant::now() + timeout;
        let work = Deadline::new(request_future, deadline);

        if let Err(spawn_err) = handle.spawn(SpawnedTransaction {
            deliverable: Some(deliverable),
            work,
            _counter: counter,
            start_time,
            task,
        }) {
            // The SpawnedTransaction should be dropped and
            // therefore should notify the deliverable.
            warn!("Failed to spawn transaction, error: {:?}", spawn_err);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate env_logger;

    use hyper;
    use hyper::Request;
    use hyper_http_connector::HttpConnector;
    use hyper_tls::HttpsConnector;
    use native_tls::TlsConnector;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use tokio::runtime::current_thread::{Handle, Runtime};
    use tokio::timer::Delay;

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
                let transaction = Transaction::new(
                    self.counter.clone(),
                    Request::get("https://onesignal.com/")
                        .body(Body::empty())
                        .unwrap(),
                    false,
                );
                transaction.spawn_request(
                    &self.client,
                    &self.handle,
                    Duration::from_secs(10),
                    Counter::new(),
                );
            }

            Ok(Async::Ready(()))
        }
    }

    fn test_hyper_client() -> hyper::Client<HttpsConnector<HttpConnector>> {
        let tls = TlsConnector::builder()
            .and_then(|builder| builder.build())
            .unwrap();
        let mut http = HttpConnector::new(4);
        http.enforce_http(false);
        let connector = HttpsConnector::from((http, tls));
        hyper::Client::builder().build(connector)
    }

    #[test]
    fn unfinished_transactions_get_sent_to_deliverable() {
        let _ = env_logger::try_init();

        let counter = DeliveryCounter::new();
        let counter_clone = counter.clone();
        let join_handle = thread::spawn(move || {
            let mut runtime = Runtime::new().expect("Able to create current_thread::Runtime");
            let handle = runtime.handle();

            let client = test_hyper_client();
            let work = SpawnTransactionsFuture {
                client,
                counter: counter_clone,
                handle,
            }.and_then(|()| {
                let when = Instant::now() + Duration::from_secs(3);
                Delay::new(when)
                    .map_err(|err| panic!("Error on delay: {:?}", err))
                    .and_then(|_| Ok(()))
            });

            // Run the transactions until the core finishes
            let _ = runtime.block_on(work);
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
            let mut runtime = Runtime::new().expect("Able to create current_thread::Runtime");
            let handle = runtime.handle();

            let client = test_hyper_client();
            let work = SpawnTransactionsFuture {
                client,
                counter: counter_clone,
                handle,
            }.and_then(|()| {
                let when = Instant::now() + Duration::from_secs(3);
                Delay::new(when)
                    .map_err(|err| panic!("Error on delay: {:?}", err))
                    .and_then(|_| -> Result<(), _> {
                        panic!("Hahaha, I will panic now.");
                    })
            });

            // Run the transactions until the core finishes
            let _ = runtime.block_on(work);
        });

        let _ = join_handle.join();

        assert_ne!(counter.response_count(), TRANSACTION_SPAWN_COUNT);
        assert_eq!(counter.total_count(), TRANSACTION_SPAWN_COUNT);
    }
}
