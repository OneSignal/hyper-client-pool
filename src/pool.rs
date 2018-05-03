//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::any::Any;
use std::cmp;
use std::fmt;
use std::mem;
use std::thread;
use std::time::Duration;

use hyper::Client;

use config::Config;
use dispatcher::Dispatcher;

/// Pool of HTTP clients
///
/// Manages a set of `hyper::Client` for maximizing throughput while presenting
/// a `request` API similar to using a `hyper::Client` directly. The number of
/// active transactions running on each client is tracked so that max sockets
/// may be respected. When all clients are full, backpressure is provided in the
/// form of an Error variant saying "busy; try again later".
pub struct Pool<D: Dispatcher> {
    clients: Vec<Client<D>>,
    client_index: usize,
    config: Config,
}

pub enum Error<D: Dispatcher> {
    /// The pool is processing the maximum number of transactions
    Full {
        url: Url,
        transaction: Transaction<D>,
    },

    /// It seems that hyper client error won't always return the stuff passed
    /// into it. Not sure what this means, but we don't have the transaction and
    /// url any longer :(.
    LostToEther
}

impl<D: Dispatcher> fmt::Debug for Error<D> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Full { .. } => {
                write!(f, "Full {{ .. }}")
            },
            Error::LostToEther => {
                f.debug_struct("LostToEther").finish()
            }
        }
    }
}

impl<D: Dispatcher + Any> ::std::error::Error for Error<D> {
    fn cause(&self) -> Option<&::std::error::Error> {
        None
    }

    fn description(&self) -> &str {
        match *self {
            Error::Full { .. } => "client pool at max capacity",
            Error::LostToEther => "passed to hyper client but something bad happened",
        }
    }
}

impl<D: Dispatcher + Any> ::std::fmt::Display for Error<D> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "{}", ::std::error::Error::description(self))
    }
}

impl<D: Dispatcher> Pool<D> {
    /// Creat a new pool according to config
    pub fn new(mut config: Config) -> Pool<D> {
        // Make sure config.workers is a reasonable value
        let num_workers = cmp::max(1, config.workers);
        config.workers = num_workers;

        // Ditto for max_sockets
        let max_sockets = cmp::max(1, config.max_sockets);
        config.max_sockets = max_sockets;

        let clients = (0..num_workers)
            .map(|_| Client::new(&config))
            .collect();

        Pool {
            clients: clients,
            client_index: 0,
            config: config,
        }
    }

    /// Start or queue a request
    ///
    /// The request will be started immediately assuming one of the clients in
    /// this pool is not at max_sockets.
    pub fn request(
        &mut self,
        url: Url,
        transaction: Transaction<D>
    ) -> Result<(), Error<D>> {
        // Round robin requests to clients. This assumes a busy server where most clients will have
        // a decent amount of work and will actually benefit from distributing requests.
        let mut count = 0;
        loop {
            self.client_index = (self.client_index + 1) % self.clients.len();
            let index = self.client_index;

            {
                let client = &mut self.clients[index];

                if !client.is_full() {
                    match client.request(url, transaction) {
                        Err(ClientError::Disconnected { url, handler: pooled }) => {
                            // Client disconnected; create a new one to replace it
                            warn!("hyper::Client disconnected unexpectedly; \
                                   replacing with new client");
                            ::std::mem::replace(client, Client::new(&self.config));

                            let transaction = pooled.into_transaction();

                            // Try and start the request again. Just give up if it still doesn't
                            // work
                            match client.request(url, transaction) {
                                Err(_err) => {
                                    warn!("new client was unable to service single request");
                                    return Err(Error::LostToEther);
                                },
                                Ok(()) => return Ok(()),
                            }
                        },
                        Err(ClientError::ShutdownDisconnection) => {
                            // this should be unreachable
                            warn!("hyper client pool got ShutdownDisconnection");
                            return Err(Error::LostToEther);
                        },
                        Err(ClientError::EventLoopFull) => {
                            // rotor eats these errors. typing this out makes me realize how leaky
                            // this abstraction currently is.
                            return Err(Error::LostToEther);
                        },
                        Err(ClientError::EventLoopClosed) => {
                            warn!("hyper::Client event loop closed; replacing with new client");
                            ::std::mem::replace(client, Client::new(&self.config));
                            return Err(Error::LostToEther);
                        },
                        Err(ClientError::EventLoopIo) => {
                            error!("hyper::Client io::Error when notifying event loop");
                            return Err(Error::LostToEther);
                        },
                        Ok(_) => return Ok(()),
                    }
                }
            }

            count += 1;
            if count == self.clients.len() {
                break;
            }
        }

        Err(Error::Full {
            url: url,
            transaction: transaction
        })
    }

    /// Shutdown the pool
    ///
    /// Waits for all workers to be empty before stopping.
    pub fn shutdown(&mut self) {
        let duration = Duration::from_millis(50);

        let clients = mem::replace(&mut self.clients, Vec::new());
        for client in clients {
            let mut remaining = client.active_transactions();
            while remaining != 0 {
                debug!("Waiting for {} transactions in current client", remaining);

                thread::sleep(duration);
                remaining = client.active_transactions();
            }

            let hyper_client = client.into_inner();
            hyper_client.close();
        }
    }
}


#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::sync::atomic::{Ordering, AtomicUsize};

    use hyper::Method;
    use hyper::Url;

    use ::{Transaction, Dispatcher, DeliveryResult};
    use super::{Config, Pool, Error};

    #[derive(Clone)]
    struct CompletionCounter(Arc<AtomicUsize>);

    impl Dispatcher for CompletionCounter {
        fn notify(self, _result: DeliveryResult) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl CompletionCounter {
        pub fn new() -> CompletionCounter {
            CompletionCounter(Arc::new(AtomicUsize::new(0)))
        }

        pub fn value(&self) -> usize {
            self.0.load(Ordering::Acquire)
        }
    }


    #[test]
    fn lots_of_get_single_worker() {
        let mut config = Config::default();
        config.workers = 1;

        let mut pool = Pool::new(config);
        let (tx, rx) = mpsc::channel();

        for _ in 0..5 {
            pool.request(
                Url::parse("https://www.httpbin.org").unwrap(),
                Transaction::new(tx.clone(), Method::Get, Default::default(), None)
            ).expect("request ok");
        }

        assert_eq!(pool.clients[0].active_transactions(), 5);

        let mut received = 0;
        while let Ok(result) = rx.recv() {
            println!("got result: {:?}\n\n", result);

            received += 1;
            if received == 5 {
                break;
            }
        }
    }

    #[test]
    fn graceful_shutdown() {
        let txn = 20;
        let counter = CompletionCounter::new();

        let mut config = Config::default();
        config.workers = 2;

        let mut pool = Pool::new(config);

        for _ in 0..txn {
            pool.request(
                Url::parse("https://www.httpbin.org").unwrap(),
                Transaction::new(counter.clone(), Method::Get, Default::default(), None)
            ).expect("request ok");
        }

        pool.shutdown();
        assert_eq!(counter.value(), txn);
    }

    #[test]
    fn full_error() {
        let mut config = Config::default();
        config.workers = 1;
        config.max_sockets = 1;

        let mut pool = Pool::new(config);
        let (tx, rx) = mpsc::channel();

        // Start first request
        pool.request(
            Url::parse("https://www.httpbin.org").unwrap(),
            Transaction::new(tx.clone(), Method::Get, Default::default(), None)
        ).expect("request ok");

        // Should be counted
        assert_eq!(pool.clients[0].active_transactions(), 1);

        match pool.request(
            Url::parse("https://www.httpbin.org").unwrap(),
            Transaction::new(tx.clone(), Method::Get, Default::default(), None)
        ) {
            Err(Error::Full { .. }) => (),
            res => panic!("got expected Error::Full; got {:?}", res),
        }

        rx.recv().unwrap();
    }
}
