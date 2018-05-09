//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::cmp;

use fpool::{ActResult, RoundRobinPool};

use config::Config;
use dispatcher::Dispatcher;
use error::Error;
use executor::{Executor, SendError, SpawnError, ExecutorHandle};
use transaction::Transaction;

/// Pool of HTTP clients
///
/// Manages a set of `hyper::Client` for maximizing throughput while presenting
/// a `request` API similar to using a `hyper::Client` directly. The number of
/// active transactions running on each client is tracked so that max sockets
/// may be respected. When all clients are full, backpressure is provided in the
/// form of an Error variant saying "busy; try again later".
pub struct Pool<D: Dispatcher> {
    executor_handles: RoundRobinPool<ExecutorHandle<D>, Error<D>>,
}

impl<D: Dispatcher> Pool<D> {
    /// Creat a new pool according to config
    pub fn new(mut config: Config) -> Result<Pool<D>, Error<D>> {
        // Make sure config.workers is a reasonable value
        let num_workers = cmp::max(1, config.workers);
        config.workers = num_workers;

        let executor_handles = RoundRobinPool::builder(config.workers, move || {
            Executor::spawn(&config)
                .map_err(|err| {
                    match err {
                        SpawnError::ThreadSpawn(err) => Error::ThreadSpawn(err),
                        SpawnError::HttpsConnector(err) => Error::HttpsConnector(err),
                    }
                })
        }).build()?;

        Ok(Pool {
            executor_handles,
        })
    }

    /// Start or queue a request
    ///
    /// The request will be started immediately assuming one of the clients in
    /// this pool is not at max_sockets.
    pub fn request(
        &mut self,
        transaction: Transaction<D>,
    ) -> Result<(), Error<D>> {
        self.executor_handles.act(move |handle| {
            match handle.send(transaction) {
                Err(SendError::Full(transaction)) => {
                    ActResult::ValidWithError(Error::Full(transaction))
                },
                Err(SendError::FailedSend(transaction)) => {
                    // Recreate the executor / thread if it failed to send
                    ActResult::InvalidWithError(Error::FailedSend(transaction))
                },
                _ => ActResult::Valid,
            }
        })
    }

    /// Shutdown the pool
    ///
    /// Waits for all workers to be empty before stopping.
    pub fn shutdown(self) {
        let handles = self.executor_handles.into_items();
        let join_handles : Vec<_> = handles.into_iter()
            .map(|handle| handle.send_shutdown())
            .collect();

        for join_handle in join_handles.into_iter() {
            let _ = join_handle.join();
        }
    }
}
