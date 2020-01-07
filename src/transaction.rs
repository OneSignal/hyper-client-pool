use std::fmt;
use std::io::{self, ErrorKind};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::prelude::*;
use hyper::client::connect::Connect;
use hyper::{self, Client, Request};
use hyper::{Body, Response};

use crate::deliverable::Deliverable;
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

impl<D: Deliverable> Transaction<D> {
    pub fn new(deliverable: D, request: Request<Body>, requires_body: bool) -> Transaction<D> {
        Transaction {
            deliverable,
            request,
            requires_body,
        }
    }

    pub(crate) fn spawn_request<C: 'static + Connect + Clone + Send + Sync>(
        self,
        client: Arc<Client<C>>,
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

        let request_future = async move {
            match client.request(request).await {
                Ok(response) => {
                    if requires_body {
                        let (parts, mut body) = response.into_parts();
                        let mut body_vec = Vec::new();

                        while let Some(Ok(chunk)) = body.next().await {
                            body_vec.extend_from_slice(&*chunk);
                        }

                        let body_size = body_vec.len();
                        Ok((
                            Response::from_parts(parts, Body::empty()),
                            Some(body_vec),
                            body_size,
                        ))
                    } else {
                        // Note that you must consume the body if you want keepalive
                        // to take affect.
                        let (parts, mut body) = response.into_parts();

                        let mut body_len = 0;

                        while let Some(Ok(chunk)) = body.next().await {
                            body_len += chunk.len();
                        }

                        Ok((Response::from_parts(parts, Body::empty()), None, body_len))
                    }
                }
                Err(e) => Err(e),
            }
        };

        let request_future = async move {
            let result = tokio::time::timeout(timeout, request_future).await;
            let duration = start_time.elapsed();

            let delivery_result = match result {
                Ok(Ok((response, body, body_size))) => {
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

                Ok(Err(hyper_error)) => {
                    trace!(
                        "Transaction errored during delivery, error: {:?}, duration: {:?}",
                        hyper_error,
                        duration
                    );
                    DeliveryResult::HyperError {
                        error: hyper_error,
                        duration,
                    }
                }

                Err(timer_err) => {
                    trace!(
                        "Timer around Transaction errored, error: {:?}, duration: {:?}",
                        timer_err,
                        duration
                    );
                    DeliveryResult::TimeoutError {
                        error: io::Error::new(ErrorKind::Other, timer_err),
                        duration,
                    }
                }
            };

            deliverable.complete(delivery_result);

            drop(counter);
        };

        tokio::spawn(request_future);
    }
}

#[cfg(test)]
mod tests {
    extern crate env_logger;

    use hyper;
    use hyper::client::connect::HttpConnector;
    use hyper::Request;
    use hyper_tls::HttpsConnector;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::time::delay_for;

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

    const TRANSACTION_SPAWN_COUNT: usize = 200;

    async fn make_requests<C>(client: Client<C>, counter: &DeliveryCounter)
    where
        C: 'static + Connect + Clone + Send + Sync,
    {
        let client = Arc::new(client);

        for _ in 0..TRANSACTION_SPAWN_COUNT {
            let transaction = Transaction::new(
                counter.clone(),
                Request::get("https://onesignal.com/")
                    .body(Body::empty())
                    .unwrap(),
                false,
            );
            transaction.spawn_request(Arc::clone(&client), Duration::from_secs(10), Counter::new());
        }
    }

    fn test_hyper_client() -> hyper::Client<HttpsConnector<HttpConnector>> {
        let connector = HttpsConnector::new();
        hyper::Client::builder().build(connector)
    }

    #[tokio::test]
    async fn unfinished_transactions_get_sent_to_deliverable() {
        let _ = env_logger::try_init();

        let counter = DeliveryCounter::new();

        let client = test_hyper_client();

        make_requests(client, &counter).await;
        delay_for(Duration::from_secs(3)).await;

        assert_ne!(counter.response_count(), TRANSACTION_SPAWN_COUNT);
        assert_eq!(counter.total_count(), TRANSACTION_SPAWN_COUNT);
    }

    #[tokio::test]
    async fn panicked_transactions_get_sent_to_deliverable() {
        let _ = env_logger::try_init();

        let counter = DeliveryCounter::new();

        let client = test_hyper_client();
        make_requests(client, &counter).await;
        delay_for(Duration::from_secs(3)).await;

        assert_ne!(counter.response_count(), TRANSACTION_SPAWN_COUNT);
        assert_eq!(counter.total_count(), TRANSACTION_SPAWN_COUNT);
    }
}
