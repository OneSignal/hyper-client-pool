//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::time::Duration;
use std::cmp;
use std::sync::Arc;
use std::fmt;
use std::marker::Reflect;
use std::mem;

use hyper::client::{Request, Handler, Response, DefaultTransport};
use hyper::{Next, Encoder, Decoder};
use hyper;
use hyper::Url;

use super::{Transaction, Deliverable};

/// This is essentially an AtomicUsize that is clonable and whose count is based
/// on the number of copies. The count is automaticaly updated on drop.
#[derive(Clone)]
struct Count(Arc<u8>);

impl Count {
    /// Get the count
    ///
    /// This method is inherently racey. Assume the count will have changed once
    /// the value is observed.
    #[inline]
    pub fn get(&self) -> usize {
        Arc::strong_count(&self.0)
    }

    /// Create a new RAII counter
    pub fn new() -> Count {
        // Note the value in the Arc doesn't matter since we rely on the Arc's
        // strong count to provide a value.
        Count(Arc::new(0))
    }
}

/// Wraps a hyper client with additional information/APIs like active transaction count
struct Client<D: Deliverable> {
    /// Number of transactions running on this client
    transactions: Count,

    /// The hyper client being wrapped
    inner: hyper::Client<PooledTransaction<D>>,

    /// Maximum active transactions,
    max_parallel_transactions: usize,
}

impl<D: Deliverable> Client<D> {
    /// Create a new client
    pub fn new(config: &Config) -> Client<D> {
        let hyper_client = hyper::Client::<PooledTransaction<D>>::configure()
            .keep_alive(true)
            .keep_alive_timeout(Some(config.keep_alive_timeout))
            .max_sockets(config.max_sockets + 100)
            .connect_timeout(config.connection_timeout)
            .build()
            .unwrap(); // TODO

        Client {
            inner: hyper_client,
            transactions: Count::new(),
            max_parallel_transactions: config.max_sockets,
        }
    }

    #[inline]
    pub fn active_transactions(&self) -> usize {
        self.transactions.get() - 1
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        let active = self.active_transactions();
        trace!("active_transactions: {}, max: {}", active, self.max_parallel_transactions);
        self.active_transactions() >= self.max_parallel_transactions
    }

    #[inline]
    fn pooled(&self, transaction: Transaction<D>) -> PooledTransaction<D> {
        PooledTransaction {
            inner: transaction,
            _count: self.transactions.clone(),
        }
    }

    fn into_inner(self) -> hyper::Client<PooledTransaction<D>> {
        self.inner
    }

    pub fn request(
        &self,
        url: Url,
        transaction: Transaction<D>
    ) -> Result<(), Option<(Url, Transaction<D>)>> {
        self.inner
            .request(url, self.pooled(transaction))
            .map_err(|err| err.recover().map(|(url, trans)| (url, trans.into_transaction())))
    }
}

/// Wrapper for transaction data
///
/// Used as an RAII tracker for connection count per client
struct PooledTransaction<D: Deliverable> {
    inner: Transaction<D>,

    /// Number of active connections on current client
    _count: Count,
}

impl<D: Deliverable> Handler<DefaultTransport> for PooledTransaction<D> {
    #[inline]
    fn on_request(&mut self, req: &mut Request) -> Next {
        self.inner.on_request(req)
    }

    #[inline]
    fn on_request_writable(&mut self, encoder: &mut Encoder<DefaultTransport>) -> Next {
        self.inner.on_request_writable(encoder)
    }

    #[inline]
    fn on_response(&mut self, res: Response) -> Next {
        self.inner.on_response(res)
    }

    #[inline]
    fn on_response_readable(&mut self, decoder: &mut Decoder<DefaultTransport>) -> Next {
        self.inner.on_response_readable(decoder)
    }

    #[inline]
    fn on_error(&mut self, err: hyper::Error) -> Next {
        self.inner.on_error(err)
    }
}

impl<D: Deliverable> PooledTransaction<D> {
    pub fn into_transaction(self) -> Transaction<D> {
        self.inner
    }
}


/// Pool of HTTP clients
///
/// Manages a set of `hyper::Client` for maximizing throughput while presenting
/// a `request` API similar to using a `hyper::Client` directly. The number of
/// active transactions running on each client is tracked so that max sockets
/// may be respected. When all clients are full, backpressure is provided in the
/// form of an Error variant saying "busy; try again later".
pub struct Pool<D: Deliverable> {
    clients: Vec<Client<D>>,
    client_index: usize,
    _max_parallel_transactions: usize,
}

pub struct Config {
    /// How long to keep a connection alive before timing out
    pub keep_alive_timeout: Duration,

    /// Connection timeout (in seconds)
    pub connection_timeout: Duration,

    /// Maximum sockets per worker
    pub max_sockets: usize,

    /// Number of workers in the pool
    pub workers: usize,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            keep_alive_timeout: Duration::from_secs(300),
            connection_timeout: Duration::from_secs(60),
            max_sockets: 10_000,
            workers: 8,
        }
    }
}

pub enum Error<D: Deliverable> {
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

impl<D: Deliverable> fmt::Debug for Error<D> {
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

impl<D: Deliverable + Reflect> ::std::error::Error for Error<D> {
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

impl<D: Deliverable + Reflect> ::std::fmt::Display for Error<D> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "{}", ::std::error::Error::description(self))
    }
}

impl<D: Deliverable> Pool<D> {
    /// Creat a new pool according to config
    pub fn new(config: &mut Config) -> Pool<D> {
        // Make sure config.workers is a reasonable value
        let num_workers = cmp::max(1, config.workers);
        config.workers = num_workers;

        // Ditto for max_sockets
        let max_sockets = cmp::max(1, config.max_sockets);
        config.max_sockets = max_sockets;

        // Maximum number of active transactions would be using each socket on
        // each worker.
        let max_parallel_transactions = max_sockets * num_workers;

        let clients = (0..num_workers)
            .map(|_| Client::new(&config))
            .collect();

        Pool {
            clients: clients,
            client_index: 0,
            _max_parallel_transactions: max_parallel_transactions
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
        // This strategy will favor clients earlier in the list over clients
        // later in the list.  There's pros/cons to this vs load balancing, but
        // my current justification for this approach is 1) it's easy to write,
        // and 2) results in fewer context switches, and 3) event loops are good
        // at lots of parallel activity.
        let mut count = 0;
        loop {
            let client = &self.clients[self.client_index];
            self.client_index = (self.client_index + 1) % self.clients.len();

            if !client.is_full() {
                try!(client
                    .request(url, transaction)
                    .map_err(|err| {
                        err.map(|(url, transaction)| {
                            Error::Full { url: url, transaction: transaction }
                        }).unwrap_or(Error::LostToEther)
                    }));
                return Ok(());
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
        use std::thread;
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
    use std::sync::atomic::{Ordering, AtomicUsize};
    use std::sync::Arc;

    use hyper::Url;
    use hyper::method::Method;

    use super::{Config, Pool, Error};
    use ::{Transaction, Deliverable, DeliveryResult};

    #[derive(Clone)]
    struct CompletionCounter(Arc<AtomicUsize>);

    impl Deliverable for CompletionCounter {
        fn complete(self, _result: DeliveryResult) {
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

        let mut pool = Pool::new(&mut config);
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

        let mut pool = Pool::new(&mut config);

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

        let mut pool = Pool::new(&mut config);
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
