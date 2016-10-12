pub extern crate hyper;
#[macro_use] extern crate log;

use std::io;
use std::time::{Instant, Duration};
use std::fmt;
use std::mem;

use hyper::client::{Request, Handler, Response, DefaultTransport};
use hyper::{Next, Encoder, Decoder};
use hyper::method::Method;
use hyper::Headers;

pub mod pool;

pub trait Deliverable : Send + 'static {
    fn complete(self, result: DeliveryResult);
}

pub use pool::Pool;

#[derive(Debug)]
pub struct Transaction<D>
    where D: Deliverable
{
    start_time: Instant,
    state: State,
    request_body: Option<Vec<u8>>,
    written: usize,
    response_body: Vec<u8>,
    response: Option<Response>,
    deliverable: Option<D>,
}

pub enum DeliveryResult {
    Unsent {
        duration: Duration,
    },

    Response {
        inner: Response,
        body: Vec<u8>,
        duration: Duration
    },

    IoError {
        error: io::Error,
        response: Option<Response>,
        duration: Duration,
    },

    HyperError {
        error: hyper::Error,
        response: Option<Response>,
        duration: Duration,
    }
}

impl fmt::Debug for DeliveryResult {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            DeliveryResult::Response { ref inner, ref body, ref duration } => {
                f.debug_struct("DeliveryResult::Response")
                    .field("inner", inner)
                    .field("body", &::std::str::from_utf8(&body[..]))
                    .field("duration", duration)
                    .finish()
            },
            DeliveryResult::IoError { ref error, ref response, ref duration } => {
                f.debug_struct("DeliveryResult::IoError")
                    .field("error", error)
                    .field("response", response)
                    .field("duration", duration)
                    .finish()
            },
            DeliveryResult::HyperError { ref error, ref response, ref duration } => {
                f.debug_struct("DeliveryResult::HyperError")
                    .field("error", error)
                    .field("response", response)
                    .field("duration", duration)
                    .finish()
            },
            DeliveryResult::Unsent { ref duration } => {
                f.debug_struct("DeliveryResult::Unsent")
                    .field("duration", duration)
                    .finish()
            }
        }
    }
}

impl<D> Transaction<D>
    where D: Deliverable
{
    pub fn new(deliverable: D,
               method: Method,
               headers: Headers,
               body: Option<Vec<u8>>)
        -> Transaction<D>
    {
        Transaction {
            start_time: Instant::now(),
            state: New { method: method, headers: headers },
            request_body: body,
            written: 0,
            response_body: Vec::new(),
            response: None,
            deliverable: Some(deliverable)
        }
    }
}

impl<D> Drop for Transaction<D>
    where D: Deliverable
{
    fn drop(&mut self) {
        let state = mem::replace(&mut self.state, Sending); // don't care what state is anymore
        let response = self.response.take();
        let response_body = mem::replace(&mut self.response_body, Vec::new());
        let deliverable = self.deliverable.take().unwrap();

        let duration = self.start_time.elapsed();

        let response = match state {
            New { .. } | Sending => DeliveryResult::Unsent { duration: duration },
            IoError { error } => {
                DeliveryResult::IoError {
                    error: error,
                    response: response,
                    duration: duration,
                }
            },
            HyperError { error } => {
                DeliveryResult::HyperError {
                    error: error,
                    response: response,
                    duration: duration,
                }
            },
            Receiving => {
                DeliveryResult::Response {
                    inner: response.unwrap(),
                    body: response_body,
                    duration: duration
                }
            },
        };

        deliverable.complete(response);
    }
}

#[derive(Debug)]
enum State {
    /// New, unsent request
    ///
    /// `initfn` here is working around a deficit in the hyper API where only
    /// one header can be set at a time instead of a whole replacement of the
    /// headers.
    // need way to provide r
    New { method: Method, headers: Headers },

    /// Making request without body
    Sending,

    /// Receiving response
    Receiving,

    /// Got an io::Error and cannot complete
    IoError { error: io::Error },

    /// Got a hyper error and cannot be complete
    HyperError { error: hyper::Error },
}

use self::State::*;

#[inline]
fn read() -> Next {
    Next::read().timeout(Duration::from_secs(10))
}

impl<D> Handler<DefaultTransport> for Transaction<D>
    where D: Deliverable,
{
    fn on_request(&mut self, req: &mut Request) -> Next {
        let state = ::std::mem::replace(&mut self.state, State::Sending);
        match state {
            New { method, headers } => {

                // Set provided headers on the hyper request.
                //
                // Replacing the default headers isn't exactly supported out of
                // the box. One workaround is using extend() which requires
                // downcasting and clones, or there's this version which
                // requires grabbing the only header originally set, adding it
                // to ours, and setting them as the request headers.
                let mut original = ::std::mem::replace(req.headers_mut(), headers);
                let host = original.remove::<hyper::header::Host>();
                if let Some(host) = host {
                    req.headers_mut().set(host);
                }

                // Set the request method
                req.set_method(method);

                // Request write interest if there's a request body.
                if self.request_body.is_some() {
                    Next::write()
                } else {
                    read()
                }
            },
            Sending => Next::write(),
            _ => Next::read(),
        }
    }

    fn on_request_writable(&mut self, encoder: &mut Encoder<DefaultTransport>) -> Next {
        match (&self.state, self.request_body.as_ref()) {
            (&Sending, Some(ref body)) => {
                match encoder.write(&body[self.written..]) {
                    Ok(n) => {
                        self.written += n;
                    },
                    Err(err) => match err.kind() {
                        io::ErrorKind::WouldBlock => (),
                        _ => {
                            // Unrecoverable?
                            self.state = IoError { error: err };
                            return Next::end()
                        }
                    }
                }

                if self.written != body.len() {
                    return Next::write();
                }
            },
            _ => ()
        }

        encoder.close();
        read()
    }

    fn on_response(&mut self, res: Response) -> Next {
        self.response = Some(res);
        self.state = Receiving;

        // Assume there is a body to read; should just read 0 if not.
        read()
    }

    fn on_response_readable(&mut self, decoder: &mut Decoder<DefaultTransport>) -> Next {
        let mut buf: [u8; 2048] = unsafe { ::std::mem::uninitialized() };

        loop {
            match decoder.read(&mut buf[..]) {
                // End of body
                Ok(0) => {
                    return Next::end()
                }

                // Continue reading
                Ok(bytes) => {
                    self.response_body.extend_from_slice(&buf[..bytes]);
                },

                // io::Error
                Err(err) => match err.kind() {
                    io::ErrorKind::WouldBlock => {
                        return read()
                    },
                    _ => {
                        self.state = IoError { error: err };
                        return Next::end()
                    }
                }
            }
        }
    }

    fn on_error(&mut self, err: hyper::Error) -> Next {
        self.state = HyperError { error: err };
        Next::remove()
    }
}

impl Deliverable for ::std::sync::mpsc::Sender<DeliveryResult> {
    fn complete(self, result: DeliveryResult) {
        let _ = self.send(result);
    }
}

#[cfg(test)]
mod tests {
    extern crate env_logger;

    use std::time::Duration;
    use std::sync::mpsc;

    use hyper::client::Client;
    use hyper::method::Method;
    use hyper::{Headers, Url};

    use super::{DeliveryResult, Transaction};

    #[test]
    fn it_works() {
        let _ = env_logger::init();

        let (tx, rx) = mpsc::channel();
        let client = Client::<Transaction<mpsc::Sender<DeliveryResult>>>::configure()
            .keep_alive(true)
            .keep_alive_timeout(Some(Duration::from_secs(60)))
            .max_sockets(30_000)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        let url = Url::parse("https://www.httpbin.org").unwrap();

        let transaction = Transaction::new(
            tx,
            Method::Get,
            Headers::default(),
            None);


        client.request(url, transaction).unwrap();

        rx.recv().unwrap();
        client.close();
    }

    #[test]
    fn keep_alive_sockets_are_replaced() {
        let _ = env_logger::init();

        let concurrent = 5;

        let (tx, rx) = mpsc::channel();
        let client = Client::<Transaction<mpsc::Sender<DeliveryResult>>>::configure()
            .keep_alive(true)
            .keep_alive_timeout(Some(Duration::from_secs(300)))
            .max_sockets(concurrent + 1)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        for _ in 0..5 {
            // Process requests on httpbin
            for _ in 0..concurrent {
                let url = Url::parse("https://www.httpbin.org").unwrap();
                let transaction = Transaction::new(
                    tx.clone(),
                    Method::Get,
                    Headers::default(),
                    None);

                client.request(url, transaction).unwrap();
            }

            // Make sure they're all done
            for _ in 0..concurrent {
                rx.recv().unwrap();
            }

            // Now make requests somewhere else. Since the keep-alive is five
            // minutes, this should mean that socket is closed and a new one
            // opened.
            for _ in 0..concurrent {
                let url = Url::parse("https://www.google.com").unwrap();
                let transaction = Transaction::new(
                    tx.clone(),
                    Method::Get,
                    Headers::default(),
                    None);

                client.request(url, transaction).unwrap();
            }

            for _ in 0..concurrent {
                rx.recv().unwrap();
            }
        }

        client.close();
    }
}
