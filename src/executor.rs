//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::io;
use std::thread::{self, JoinHandle};

use futures::{Poll, Future, Stream, Async};
use futures::sync::mpsc as FuturesMpsc;
use tokio_core::reactor::{Core, Handle};

use counter::Counter;
use dispatcher::Dispatcher;
use transaction::Transaction;

/// Lives on a separate thread and runs Transactions sent by Pool
struct Executor<D: Dispatcher> {
    index: usize,
    max_parallel_transactions: usize,
    handle: Handle,
    receiver: FuturesMpsc::UnboundedReceiver<(Transaction<D>, Counter)>,
}

struct ExecutorHandle<D: Dispatcher> {
    /// Number of transactions running on this executor thread
    /// Because we're holding on to a copy, this is actually 1
    /// larger than the number of transactions
    transaction_counter: Counter,

    sender: FuturesMpsc::UnboundedSender<(Transaction<D>, Counter)>,
    join_handle: JoinHandle<()>,
}

impl<D: Dispatcher> Executor<D> {
    pub fn spawn(
        index: usize,
        max_parallel_transactions: usize,
    ) -> Result<ExecutorHandle<D>, io::Error> {
        let (tx, rx) = FuturesMpsc::unbounded();

        let counter = Counter::new();

        info!("Spawning Executor({}).", index);

        thread::Builder::new()
            .name(format!("Hyper-Client-Pool Executor ({})", index))
            .spawn(move || {
                let mut core = Core::new().unwrap();
                let executor = Executor {
                    index,
                    max_parallel_transactions,
                    receiver: rx,
                    handle: core.handle(),
                };

                if let Err(err) = core.run(executor) {
                    warn!("Error when running Executor: {:?}", err);
                }

                info!("Executor({}) exited.", index);
            })
            .map(|join_handle| {
                ExecutorHandle {
                    transaction_counter: counter,
                    sender: tx,
                    join_handle,
                }
            })
    }
}

impl<D: Dispatcher> Future for Executor<D> {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            match self.receiver.poll() {
                Ok(Async::Ready(Some((transaction, counter)))) => {
                    info!("Executor({}): spawning transaction.", self.index);

                    transaction.spawn(self.handle, counter);
                },
                // No messages
                Ok(Async::NotReady) => return Ok(Async::NotReady),
                // All senders dropped or errored
                // (shouldn't be possible with () error type), shutdown
                Ok(Async::Ready(None)) | Err(()) => {
                    return Ok(Async::Ready(()));
                },
            }
        }
    }
}
