use std::io;
use std::time::{Instant, Duration};

use futures::{Future};
use futures::future::Either;
use hyper::{self, Response};
use hyper::client::FutureResponse;
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
    request: FutureResponse,
    timeout: Duration,
}

impl<D: Dispatcher> Transaction<D> {
    pub fn spawn(self, handle: Handle, counter: Counter) {
        let Transaction { dispatcher, request, timeout } = self;

        let start_time = Instant::now();
        match Timeout::new(timeout, &handle) {
            Err(error) => {
                dispatcher.notify(DeliveryResult::TimeoutError {
                    error,
                    duration: start_time.elapsed(),
                });
                warn!("Could not create timeout on handle for hyper_client_pool::Transaction");
            },
            Ok(timeout) => {
                let timed_request = request.select2(timeout).then(move |res| {
                    // Hold onto counter until this point to count the transaction
                    let _counter = counter;

                    let duration = start_time.elapsed();
                    match res {
                        // Got response
                        Ok(Either::A((response, _timeout))) => {
                            dispatcher.notify(DeliveryResult::Response {
                                inner: response,
                                duration,
                            });
                        },
                        // Request timed out
                        Ok(Either::B((_timeout_error, _request))) => {
                            dispatcher.notify(DeliveryResult::Timeout {
                                duration,
                            });
                        },
                        // Request errored
                        Err(Either::A((request_error, _timeout))) => {
                            dispatcher.notify(DeliveryResult::HyperError {
                                error: request_error,
                                duration,
                            });
                        },
                        // Timeout errored
                        Err(Either::B((timeout_error, _request))) => {
                            dispatcher.notify(DeliveryResult::TimeoutError {
                                error: timeout_error,
                                duration,
                            });
                        },
                    }
                    Ok(())
                });

                handle.spawn(timed_request);
            }
        }
    }
}
