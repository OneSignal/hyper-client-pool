use std::fmt;
use std::io;
use std::time::{Instant, Duration};

use futures::{Future, task};
use futures::future::Either;
use hyper_tls::HttpsConnector;
use hyper::{self, Response, Request, Client};
use hyper::client::HttpConnector;
use tokio_core::reactor::{Handle, Timeout};

use counter::Counter;
use dispatcher::Dispatcher;

/// The result of the transaction, a message sent to the
/// dispatcher.
///
/// This must be sent to the dispatcher in any case
/// in order to prevent data loss.
#[derive(Debug)]
pub enum DeliveryResult {
    Response {
        inner: Response,
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

pub struct Transaction<D: Dispatcher> {
    dispatcher: D,
    request: Request,
}

impl<D: Dispatcher> fmt::Debug for Transaction<D> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Transaction {{ dispatcher: (unknown), request: {:?} }}", self.request)
    }
}

impl<D: Dispatcher> Transaction<D> {
    pub fn new(
        dispatcher: D,
        request: Request,
    ) -> Transaction<D> {
        Transaction {
            dispatcher,
            request,
        }
    }

    pub(crate) fn spawn_request(self, client: &Client<HttpsConnector<HttpConnector>>, handle: &Handle, timeout: Duration, counter: Counter) {
        let Transaction { mut dispatcher, request } = self;

        let task = task::current();
        let request_future = client.request(request);

        let start_time = Instant::now();
        match Timeout::new(timeout, handle) {
            Err(error) => {
                dispatcher.notify(DeliveryResult::TimeoutError {
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
                        Ok(Either::A((response, _timeout))) => {
                            trace!("Finished transaction with response: {:?}, duration: {:?}", response, duration);
                            dispatcher.notify(DeliveryResult::Response {
                                inner: response,
                                duration,
                            });
                        },
                        // Request timed out
                        Ok(Either::B((_timeout_error, _request))) => {
                            trace!("Finished transaction with timeout, duration: {:?}", duration);
                            dispatcher.notify(DeliveryResult::Timeout {
                                duration,
                            });
                        },
                        // Request errored
                        Err(Either::A((request_error, _timeout))) => {
                            trace!("Transaction errored during hyper, error: {}, duration: {:?}", request_error, duration);
                            dispatcher.notify(DeliveryResult::HyperError {
                                error: request_error,
                                duration,
                            });
                        },
                        // Timeout errored
                        Err(Either::B((timeout_error, _request))) => {
                            trace!("Transaction errored during timeout, error: {}, duration: {:?}", timeout_error, duration);
                            dispatcher.notify(DeliveryResult::TimeoutError {
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
