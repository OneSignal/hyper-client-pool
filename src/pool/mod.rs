//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::cmp;

use fpool::RoundRobinPool;

use config::Config;
use deliverable::Deliverable;
use error::{Error, ErrorKind, RequestError, SpawnError};
use executor::{Executor, ExecutorHandle};
use transaction::Transaction;
use util::RwLockExt;

mod builder;

pub use self::builder::PoolBuilder;

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

    pub(in pool) fn new(builder: PoolBuilder<D>) -> Result<Pool<D>, SpawnError> {
        let PoolBuilder {
            mut config,
            transaction_counters,
            ..
        } = builder;

        // Make sure config.workers is a reasonable value
        let num_workers = cmp::max(1, config.workers);
        config.workers = num_workers;

        let executor_handles = RoundRobinPool::builder(config.workers, move || {
            let executor = Executor::spawn(&config);

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
        }).build()?;

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
    pub fn shutdown(self) {
        let handles = self.executor_handles.into_items();
        let join_handles: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.shutdown())
            .collect();

        for join_handle in join_handles.into_iter() {
            let _ = join_handle.join();
        }
    }
}
