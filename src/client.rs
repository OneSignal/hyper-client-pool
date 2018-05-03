//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::any::Any;
use std::cmp;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::thread;
use std::time::Duration;

use hyper;
use hyper::client::HttpConnector;
use tokio_core::reactor::Handle;

use config::Config;
use counter::Counter;
use dispatcher::Dispatcher;
use transaction::Transaction;

/// Wraps a hyper client with additional information/APIs like active transaction count
struct Client<D: Dispatcher> {
    /// Number of transactions running on this client
    transactions: Counter,

    /// The hyper client being wrapped
    inner: hyper::Client<HttpConnector>,

    max_parallel_transactions: usize,
    handle: Handle,

    _d: PhantomData<D>,
}

impl<D: Dispatcher> Client<D> {
    /// Create a new client
    pub fn new(config: &Config, handle: Handle) -> Client<D> {
        let hyper_client = hyper::Client::configure()
            .keep_alive(true)
            .keep_alive_timeout(Some(config.keep_alive_timeout))
            // TODO (darren): what to do about max_sockets?
            // joe says we can just ignore this, lol...
            // TODO (darren): what to do about connect_timeout?
            // TODO (darren): what to do about dns_workers?
            // we can specify this by creating HttpConnector ourselves
            .build(handle);

        Client {
            inner: hyper_client,
            transactions: Counter::new(),
            max_parallel_transactions: config.max_transactions(),
            handle,

            _d: PhantomData,
        }
    }

    #[inline]
    pub fn active_transactions(&self) -> usize {
        // Subtract 1 because we are holding on to an
        // instance of the counter
        self.transactions.get() - 1
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        let active = self.active_transactions();
        trace!("active_transactions: {}, max: {}", active, self.max_parallel_transactions);
        self.active_transactions() >= self.max_parallel_transactions
    }

    pub fn request(
        &self,
        request: Request,
        dispatcher: D,
    ) -> Result<(), ClientError<PooledTransaction<D>>> {
        let future_response = self.inner.request(request);
        let transaction = self.pooled(Transaction::new(dispatcher, future_response))
        self.handle.spawn(transaction);
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
}

/// Wrapper for transaction data
///
/// Used as an RAII tracker for connection count per client
struct PooledTransaction<D: Dispatcher> {
    inner: Transaction<D>,

    /// Number of active connections on current client
    _count: Counter,
}
