use std::fmt;
use std::io;
use std::string;
use std::time::{Instant, Duration};

use futures::{Stream, Future, task};
use futures::future::{self, Either};
use hyper_tls::HttpsConnector;
use hyper::{self, StatusCode, Request, Client};
use hyper::client::HttpConnector;
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
        status: StatusCode,
        body: String,
        duration: Duration,
    },

    Timeout {
        duration: Duration,
    },

    TimeoutError {
        error: io::Error,
        duration: Duration,
    },

    DeliveryError {
        error: DeliveryError,
        duration: Duration,
    },
}

#[derive(Debug)]
pub enum DeliveryError {
    Hyper(hyper::Error),
    Body(string::FromUtf8Error),
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
        let Transaction { mut deliverable, request } = self;

        let task = task::current();
        let request_future = client.request(request)
            .map_err(DeliveryError::Hyper)
            .and_then(|response| {
                let status = response.status();
                response.body()
                    .map_err(DeliveryError::Hyper)
                    .fold(Vec::new(), |mut acc, chunk| {
                        acc.extend_from_slice(&*chunk);
                        future::ok::<_, DeliveryError>(acc)
                    })
                    .and_then(move |v| {
                        let body = String::from_utf8(v).map_err(DeliveryError::Body)?;
                        Ok::<_, DeliveryError>((status, body))
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
                        Ok(Either::A(((status, body), _timeout))) => {
                            trace!("Finished transaction with status: {:?}, body: {}, duration: {:?}", status, body, duration);
                            deliverable.complete(DeliveryResult::Response {
                                status,
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
                        Err(Either::A((delivery_error, _timeout))) => {
                            trace!("Transaction errored during delivery, error: {:?}, duration: {:?}", delivery_error, duration);
                            deliverable.complete(DeliveryResult::DeliveryError {
                                error: delivery_error,
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
