use std::fmt::{self, Debug};
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use http_body_util::{BodyExt, Empty};
use hyper::body::Body;
use hyper::Response;
use hyper::{self, Request};
use hyper_util::client::legacy::connect::Connect;
use hyper_util::client::legacy::Client;

use crate::deliverable::Deliverable;
use raii_counter::Counter;
use tracing::{span, trace, Instrument};

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
        response: Response<Empty<Vec<u8>>>,
        body: Option<Vec<u8>>,
        body_size: usize,
        duration: Duration,
    },

    /// Failed to connect within the timeout limit.
    Timeout { duration: Duration },

    /// Sending a request through hyper encountered an error.
    HyperError {
        error: HyperJointError,
        duration: Duration,
    },
}

// In 1.x the client was punted to a "legacy" namespace
// in a new crate, which means the error types are distinct now
#[derive(Debug)]
pub enum HyperJointError {
    Legacy(hyper_util::client::legacy::Error),
    Mainline(hyper::Error),
}

/// A container type for a [`hyper::Request`] as well as the deliverable
/// which receives the result of the request.
pub struct Transaction<D: Deliverable, B: Body + Debug + Send + 'static> {
    deliverable: D,
    request: Request<B>,
    requires_body: bool,
    span_id: Option<tracing::Id>,
}

struct DeliverableDropGuard<D: Deliverable, B: Body + Debug + Send + 'static> {
    deliverable: Option<D>,
    span_id: Option<tracing::Id>,
    _b: PhantomData<B>,
}

impl<D: Deliverable, B: Body + Debug + Send + 'static> Drop for DeliverableDropGuard<D, B> {
    fn drop(&mut self) {
        self.deliverable.take().map(|deliverable| {
            trace!(parent: self.span_id.clone(), "Dropping transaction..");
            deliverable.complete(DeliveryResult::Dropped);
        });
    }
}

impl<D: Deliverable, B: Body + Debug + Send + 'static> DeliverableDropGuard<D, B> {
    fn new(deliverable: D, span_id: Option<tracing::Id>) -> Self {
        Self {
            deliverable: Some(deliverable),
            span_id,
            _b: PhantomData,
        }
    }

    fn take(mut self) -> D {
        self.deliverable
            .take()
            .expect("take cannot be called more than once")
    }
}

impl<D: Deliverable, B: Body + Debug + Send + 'static> fmt::Debug for Transaction<D, B> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Transaction {{ deliverable: (unknown), request: {:?} }}",
            self.request
        )
    }
}

impl<D: Deliverable, B: Body + Debug + Send + 'static> Transaction<D, B>
where
    B::Data: Send,
    B::Error: std::error::Error + Send + Sync,
    B: Unpin,
{
    pub fn new(deliverable: D, request: Request<B>, requires_body: bool) -> Transaction<D, B> {
        Transaction {
            deliverable,
            request,
            requires_body,
            span_id: None,
        }
    }

    /// Report tracing events for this transaction within the `tracing::Span`
    /// with the provided ID. Most interesting of these events is the
    /// debug-level `http_request` span, which tries to have fields provided in
    /// the opentelemetry HTTP conventions document. This event will be reported
    /// wether this method is called or not, but it will be much more useful if
    /// you provide a parent span so that you can determine why the request is
    /// being made.
    ///
    /// https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/trace/semantic_conventions/http.md
    pub fn with_parent_span(mut self, span_id: impl Into<Option<tracing::Id>>) -> Self {
        self.span_id = span_id.into();

        self
    }

    pub(crate) fn spawn_request<C: 'static + Connect + Clone + Send + Sync>(
        self,
        client: Arc<Client<C, B>>,
        timeout: Duration,
        counter: Counter,
    ) {
        let Transaction {
            deliverable,
            request,
            requires_body,
            span_id,
        } = self;

        // Creating a span per transaction can be a non-trivial CPU cost so hide it behind a feature flag
        const TRANSACTION_SPAN_LEVEL: tracing::Level = if cfg!(feature = "transaction-tracing") {
            tracing::Level::INFO
        } else {
            tracing::Level::TRACE
        };

        let outer_span = span!(
            parent: span_id,
            TRANSACTION_SPAN_LEVEL,
            "http_request",
            otel.kind = "client",
            http.url = %request.uri(),
            http.host = request.uri().host().unwrap_or(""),
            http.scheme = request.uri().scheme_str().unwrap_or(""),
            http.method = request.method().as_str(),
            http.flavor = ?request.version(),
            http.status_code = tracing::field::Empty,
            http.request_content_length = tracing::field::Empty,
            outcome = tracing::field::Empty,
        );

        let deliverable_guard = DeliverableDropGuard::<D, B>::new(deliverable, outer_span.id());

        let start_time = Instant::now();

        let inner_span1 = outer_span.clone();
        let inner_span2 = outer_span.clone();

        let request_future = async move {
            trace!("Sending request");
            match client.request(request).await {
                Ok(response) => {
                    if requires_body {
                        let (parts, body) = response.into_parts();

                        match body.collect().await {
                            Ok(body) => {
                                let body = body.to_bytes().to_vec();
                                let body_size = body.len();

                                inner_span1.record("http.request_content_length", &body_size);

                                Ok((
                                    Response::from_parts(parts, http_body_util::Empty::new()),
                                    Some(body),
                                    body_size,
                                ))
                            }
                            Err(e) => Err(HyperJointError::Mainline(e)),
                        }
                    } else {
                        // Note that you must consume the body if you want keepalive
                        // to take affect.
                        let (parts, body) = response.into_parts();

                        let mut stream = body.into_data_stream();

                        let mut body_len = 0;
                        while let Some(Ok(chunk)) = stream.next().await {
                            body_len += chunk.len();
                        }

                        inner_span1.record("http.request_content_length", &body_len);

                        Ok((
                            Response::from_parts(parts, http_body_util::Empty::new()),
                            None,
                            body_len,
                        ))
                    }
                }
                Err(e) => Err(HyperJointError::Legacy(e)),
            }
        };

        tokio::spawn(
            async move {
                let result = tokio::time::timeout(timeout, request_future).await;
                let duration = start_time.elapsed();

                let delivery_result = match result {
                    Ok(Ok((response, body, body_size))) => {
                        inner_span2.record("http.status_code", &response.status().as_u16());
                        inner_span2.record("outcome", &"http success");
                        trace!(?response, ?duration, "Finished transaction",);
                        DeliveryResult::Response {
                            response,
                            body,
                            body_size,
                            duration,
                        }
                    }

                    Ok(Err(hyper_error)) => {
                        inner_span2.record("outcome", &"http error");
                        trace!(
                            error = ?hyper_error,
                            ?duration,
                            "Transaction errored during delivery",
                        );
                        DeliveryResult::HyperError {
                            error: hyper_error,
                            duration,
                        }
                    }

                    Err(_) => {
                        inner_span2.record("outcome", &"timeout");
                        trace!(
                            ?duration,
                            timeout_limit = ?timeout,
                            "Transaction timed out",
                        );
                        DeliveryResult::Timeout { duration }
                    }
                };

                deliverable_guard.take().complete(delivery_result);

                drop(counter);
            }
            .instrument(outer_span),
        );
    }
}

#[cfg(test)]
mod tests {
    extern crate tracing_subscriber;

    use hyper;
    use hyper::body::Bytes;
    use hyper::Request;
    use hyper_tls::HttpsConnector;
    use hyper_util::client::legacy::connect::{Connect, HttpConnector};
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::time::sleep;
    use tracing::info;

    use super::*;

    #[derive(Debug, Clone)]
    struct DeliveryCounter {
        total_count: Arc<AtomicUsize>,
        response_count: Arc<AtomicUsize>,
        dropped_count: Arc<AtomicUsize>,
        hyper_error_count: Arc<AtomicUsize>,
        timeout_count: Arc<AtomicUsize>,
    }

    impl DeliveryCounter {
        fn new() -> DeliveryCounter {
            DeliveryCounter {
                total_count: Arc::new(AtomicUsize::new(0)),
                response_count: Arc::new(AtomicUsize::new(0)),
                dropped_count: Arc::new(AtomicUsize::new(0)),
                hyper_error_count: Arc::new(AtomicUsize::new(0)),
                timeout_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn timeout_count(&self) -> usize {
            self.timeout_count.load(Ordering::Acquire)
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
            match result {
                DeliveryResult::Response { .. } => {
                    self.response_count.fetch_add(1, Ordering::AcqRel);
                }
                DeliveryResult::Dropped { .. } => {
                    self.dropped_count.fetch_add(1, Ordering::AcqRel);
                }
                DeliveryResult::HyperError { .. } => {
                    self.hyper_error_count.fetch_add(1, Ordering::AcqRel);
                }
                DeliveryResult::Timeout { .. } => {
                    self.timeout_count.fetch_add(1, Ordering::AcqRel);
                }
            }

            self.total_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    const TRANSACTION_SPAWN_COUNT: usize = 200;
    const TIMEOUT_COUNT: usize = 50;

    fn make_requests<C>(client: Client<C, Empty<Bytes>>, counter: &DeliveryCounter)
    where
        C: 'static + Connect + Clone + Send + Sync,
    {
        let client = Arc::new(client);

        for i in 0..TRANSACTION_SPAWN_COUNT {
            let url = if i < TIMEOUT_COUNT {
                "https://httpbin.org/delay/4"
            } else {
                "https://httpbin.org/delay/0"
            };

            let transaction = Transaction::new(
                counter.clone(),
                Request::get(url)
                    .body(http_body_util::Empty::new())
                    .unwrap(),
                false,
            );
            transaction.spawn_request(Arc::clone(&client), Duration::from_secs(2), Counter::new());
        }
    }

    fn test_hyper_client() -> Client<HttpsConnector<HttpConnector>, Empty<Bytes>> {
        let connector = HttpsConnector::new();
        Client::builder(TokioExecutor::new()).build(connector)
    }

    #[tokio::test]
    async fn timed_out_transactions_get_sent_to_deliverable() {
        let _ = tracing_subscriber::fmt::try_init();

        info!("test start");

        let counter = DeliveryCounter::new();

        let client = test_hyper_client();

        make_requests(client, &counter);
        sleep(Duration::from_secs(3)).await;

        assert_ne!(counter.response_count(), TRANSACTION_SPAWN_COUNT);
        assert!(counter.timeout_count() > TIMEOUT_COUNT);
        assert_eq!(counter.total_count(), TRANSACTION_SPAWN_COUNT);
    }
}
