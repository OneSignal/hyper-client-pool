use std::fmt;
use std::io;
use std::time::{Instant, Duration};

use futures::{Stream, Future, task};
use futures::future::{self, Either};
use hyper_tls::HttpsConnector;
use hyper::{self, Request, Client};
use hyper::client::{Response, HttpConnector};
use tokio_core::reactor::{Handle, Timeout};

use counter::Counter;
use deliverable::Deliverable;

/// The result of the transaction, a message sent to the
/// deliverable.
///
/// This must be sent to the deliverable in any case
/// in order to prevent data loss.
#[derive(Debug)]
pub enum DeliveryResult {
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
                let timed_request = request_future.select2(timeout).then(move |res| {
                    // Hold onto counter until this point to count the transaction
                    let _counter = counter;

                    let duration = start_time.elapsed();
                    match res {
                        // Got response
                        Ok(Either::A(((response, body), _timeout))) => {
                            trace!("Finished transaction with response: {:?}, duration: {:?}", response, duration);
                            deliverable.complete(DeliveryResult::Response {
                                response,
                                body,
                                duration,
                            });
                        },
                        // Request timed out
                        Ok(Either::B((_timeout_error, _request))) => {
                            trace!("Finished transaction with timeout, duration: {:?}", duration);
                            deliverable.complete(DeliveryResult::Timeout {
                                duration,
                            });
                        },
                        // Request errored
                        Err(Either::A((hyper_error, _timeout))) => {
                            trace!("Transaction errored during delivery, error: {:?}, duration: {:?}", hyper_error, duration);
                            deliverable.complete(DeliveryResult::HyperError {
                                error: hyper_error,
                                duration,
                            });
                        },
                        // Timeout errored
                        Err(Either::B((timeout_error, _request))) => {
                            trace!("Transaction errored during timeout, error: {}, duration: {:?}", timeout_error, duration);
                            deliverable.complete(DeliveryResult::TimeoutError {
                                error: timeout_error,
                                duration,
                            });
                        },
                    }

                    task.notify();
                    Ok(())
                });

                handle.spawn(timed_request);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    extern crate env_logger;

    use futures::{Poll, Async};
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

    impl Future for SpawnTransactionsFuture {
        type Item = ();
        type Error = ();

        fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
            for _ in 0..200 {
                let transaction = Transaction::new(self.counter.clone(), Request::new(Method::Get, "https://onesignal.com/".parse().unwrap()));
                transaction.spawn_request(&self.client, &self.handle, Duration::from_secs(10), Counter::new());
            }

            Ok(Async::Ready(()))
        }
    }

    #[test]
    fn unfinished_transactions_get_sent_to_dispatcher() {
        let _ = env_logger::try_init();

        let tls = TlsConnector::builder().and_then(|builder| builder.build()).unwrap();
        let counter = DeliveryCounter::new();
        let counter_clone = counter.clone();
        thread::spawn(move || {
            let mut core = Core::new().unwrap();
            let handle = core.handle();
            let handle2 = core.handle();

            let mut http = HttpConnector::new(4, &handle);
            http.enforce_http(false);
            let connector = HttpsConnector::from((http, tls));
            let client = hyper::Client::configure()
                .connector(connector)
                .build(&handle);

            let work = SpawnTransactionsFuture {
                client,
                counter: counter_clone,
                handle,
            }.and_then(|()| {
                Timeout::new(Duration::from_secs(3), &handle2).unwrap()
                    .map_err(|_err| {})
            });

            // Run the transactions until the core finishes
            let _ = core.run(work);
        });

        let iter_duration = Duration::from_millis(100);
        let start_time = Instant::now();
        while start_time.elapsed().as_secs() < 10 {
            if counter.total_count() == 100 {
                // we finished getting all transactions back
                // make sure it wasn't all responses
                assert_ne!(counter.response_count(), 100);
                return;
            }

            thread::sleep(iter_duration);
        }

        panic!("Did not receive all tranasction results after unfinished, only received: {}!", counter.total_count());
    }
}
