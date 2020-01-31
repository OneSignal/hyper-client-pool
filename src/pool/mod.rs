//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::cmp;
use std::net::IpAddr;

use fpool::RoundRobinPool;
use hyper::client::connect::dns::Name;
use tower_service::Service;

use crate::config::Config;
use crate::deliverable::Deliverable;
use crate::error::{Error, ErrorKind, RequestError, SpawnError};
use crate::executor::{Executor, ExecutorHandle};
use crate::transaction::Transaction;
use crate::util::RwLockExt;

mod builder;

pub use self::builder::{
    ConnectorAdaptor, CreateResolver, DefaultConnectorAdapator, PoolBuilder, PoolConnector,
};

/// A pool of [`hyper::Client`]s.
///
/// Manages a set of `hyper::Client` for maximizing throughput while presenting
/// a `request` API similar to using a `hyper::Client` directly. The number of
/// active transactions running on each client is tracked so that max_transactions_per_worker
/// is respected. When all clients are full, backpressure is provided in the
/// form of an Error variant saying "busy; try again later".
pub struct Pool<D: Deliverable> {
    executor_handles: RoundRobinPool<ExecutorHandle<D>, SpawnError>,
}

impl<D: Deliverable> Pool<D> {
    pub fn builder(config: Config) -> PoolBuilder<D> {
        PoolBuilder::new(config)
    }

    pub(in crate::pool) fn new<A, CR>(builder: PoolBuilder<D>) -> Result<Pool<D>, SpawnError>
    where
        A: ConnectorAdaptor<CR::Resolver>,
        A::Connect: 'static + Clone + Send + Sync,
        CR: CreateResolver,
        CR::Resolver: 'static + Clone + Send + Sync + Service<Name>,
        CR::Error: 'static + Send + Sync + std::error::Error,
        CR::Future: Send + std::future::Future<Output = Result<CR::Response, CR::Error>>,
        CR::Response: Iterator<Item = IpAddr>,
    {
        let PoolBuilder {
            mut config,
            transaction_counters,
            ..
        } = builder;

        // Make sure config.workers is a reasonable value
        let num_workers = cmp::max(1, config.workers);
        config.workers = num_workers;

        let executor_handles = RoundRobinPool::builder(config.workers, move || {
            let resolver = CR::create_resolver();

            let executor = Executor::spawn::<A, CR::Resolver>(&config, resolver);

            // Push a transaction counter to the synchronized transaction_counters
            // if executor creation was successful
            if let (Ok(ref executor), Some(ref transaction_counters)) =
                (executor.as_ref(), transaction_counters.as_ref())
            {
                transaction_counters
                    .write_ignore_poison()
                    .push(executor.transaction_counter())
            }

            executor
        })
        .build()?;

        Ok(Pool { executor_handles })
    }

    /// Start or queue a request
    ///
    /// The request will be started immediately assuming one of the clients in
    /// this pool is not at max_sockets.
    pub fn request(&mut self, transaction: Transaction<D>) -> Result<(), Error<D>> {
        let size = self.executor_handles.size();
        self.request_inner(transaction, size)
    }

    fn request_inner(&mut self, transaction: Transaction<D>, count: usize) -> Result<(), Error<D>> {
        if count == 0 {
            return Err(Error::new(ErrorKind::PoolFull, transaction));
        }

        let transaction = match self.executor_handles.get() {
            Err(spawn_err) => return Err(Error::new(ErrorKind::Spawn(spawn_err), transaction)),
            Ok(handle) => {
                match handle.send(transaction) {
                    // Returning the transaction means that we will retry in next iteration
                    Err(RequestError::PoolFull(transaction)) => transaction,
                    Err(RequestError::FailedSend(transaction)) => {
                        // invalidate the thread as it didn't send
                        handle.invalidate();
                        transaction
                    }
                    Ok(_) => return Ok(()),
                }
            }
        };

        self.request_inner(transaction, count - 1)
    }

    /// Shutdown the pool
    ///
    /// Waits for all workers to be empty before stopping.
    pub async fn shutdown(self) {
        let handles = self.executor_handles.into_items();
        let join_handles: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.shutdown())
            .collect();

        futures::future::join_all(join_handles).await;
    }
}
