use std::time::{Duration};
use std::io;

use hyper;
use hyper::{Body, Response};

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
        duration: Duration,
    },

    /// Failed to connect within the timeout limit.
    Timeout {
        duration: Duration,
    },

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
