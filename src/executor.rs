//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::mem;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures::{Poll, Future, Stream, Async};
use futures::sync::mpsc as FuturesMpsc;
use hyper_http_connector::HttpConnector;
use hyper_tls::HttpsConnector;
use hyper::{self, Client};
use native_tls::TlsConnector;
use tokio::runtime::current_thread::{Handle, Runtime};

use config::Config;
use deliverable::Deliverable;
use error::{RequestError, SpawnError};
use raii_counter::{Counter, WeakCounter};
use transaction::Transaction;

/// Lives on a separate thread running a tokio_core::Reactor
/// and runs Transactions sent by the Pool.
pub(crate) struct Executor<D: Deliverable> {
    client: Client<HttpsConnector<HttpConnector>>,
    handle: Handle,
    transaction_counter: WeakCounter,
    transaction_timeout: Duration,
    state: ExecutorState<D>,
}

/// The handle to the Executor. It lives on the Pool thread
/// and allows message passing through futures::mpsc.
pub(crate) struct ExecutorHandle<D: Deliverable> {
    transaction_counter: WeakCounter,
    max_transactions: usize,

    conns_counter: WeakCounter,
    max_connections: usize,

    sender: FuturesMpsc::UnboundedSender<ExecutorMessage<D>>,
    join_handle: JoinHandle<()>,
}

enum ExecutorState<D: Deliverable> {
    Running(FuturesMpsc::UnboundedReceiver<ExecutorMessage<D>>),
    Draining,
    Finished,
}

type ExecutorMessage<D> = (Transaction<D>, Counter, Counter);

impl<D: Deliverable> ExecutorHandle<D> {
    pub(crate) fn send(&mut self, transaction: Transaction<D>) -> Result<(), RequestError<D>> {
        if self.is_full() {
            return Err(RequestError::PoolFull(transaction));
        }

        if let Err(err) = self.sender.unbounded_send(
            (transaction, self.transaction_counter.spawn_upgrade(), self.conns_counter.spawn_upgrade())
        ) {
            let (transaction, _counter, _conns_counter) = err.into_inner();
            return Err(RequestError::FailedSend(transaction));
        }

        Ok(())
    }

    /// Shutdowns the executor by dropping the sender, returns the JoinHandle to the thread.
    pub(crate) fn shutdown(self) -> JoinHandle<()> {
        self.join_handle
    }

    fn is_full(&self) -> bool {
        self.transaction_counter.count() >= self.max_transactions
            || self.conns_counter.count() >= self.max_connections
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.transaction_counter.count(), self.conns_counter.count())
    }
}

impl<D: Deliverable> Executor<D> {
    pub fn spawn(config: &Config) -> Result<ExecutorHandle<D>, SpawnError> {
        let (tx, rx) = FuturesMpsc::unbounded();
        let weak_counter = WeakCounter::new();
        let weak_counter_clone = weak_counter.clone();
        let keep_alive_timeout = config.keep_alive_timeout;
        let transaction_timeout = config.transaction_timeout.clone();
        let dns_threads_per_worker = config.dns_threads_per_worker;
        let conns_counter = WeakCounter::new();
        let conns_counter_clone = conns_counter.clone();

        let tls = TlsConnector::builder().and_then(|builder| builder.build()).map_err(SpawnError::HttpsConnector)?;

        thread::Builder::new()
            .name(format!("Hyper-Client-Pool Executor"))
            .spawn(move || {
                let mut runtime = Runtime::new().expect("Able to create current_thread::Runtime");
                let handle = runtime.handle();

                let mut http = HttpConnector::new(dns_threads_per_worker);
                http.enforce_http(false);
                http.set_keepalive(Some(keep_alive_timeout));
                let connector = HttpsConnector::from((http, tls));
                let client = hyper::Client::builder()
                    .keep_alive(true)
                    .keep_alive_timeout(Some(keep_alive_timeout))
                    .build_with_conns_counter(connector, Some(conns_counter_clone));

                let executor = Executor {
                    state: ExecutorState::Running(rx),
                    handle,
                    transaction_counter: weak_counter_clone,
                    client,
                    transaction_timeout,
                };

                if let Err(err) = runtime.block_on(executor) {
                    warn!("Error when running Executor: {:?}", err);
                }

                info!("Executor exited.");
            })
            .map(|join_handle| {
                ExecutorHandle {
                    transaction_counter: weak_counter,
                    max_transactions: config.max_transactions_per_worker,
                    conns_counter,
                    max_connections: config.max_connections_per_worker,
                    sender: tx,
                    join_handle,
                }
            })
            .map_err(SpawnError::ThreadSpawn)
    }
}

impl<D: Deliverable> Future for Executor<D> {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            // If self.state is not set, then it will be finished
            // so should only be not set if Finished
            let state = mem::replace(&mut self.state, ExecutorState::Finished);
            let mut state_changed = false;

            self.state = match state {
                ExecutorState::Running(mut receiver) => {
                    loop {
                        match receiver.poll() {
                            Ok(Async::Ready(Some((transaction, counter, conn_counter)))) => {
                                trace!("Executor: spawning transaction.");

                                transaction.spawn_request(
                                    &self.client,
                                    &self.handle,
                                    self.transaction_timeout.clone(),
                                    counter,
                                    conn_counter,
                                );
                            },
                            // No messages
                            Ok(Async::NotReady) => break ExecutorState::Running(receiver),
                            // All senders dropped or errored
                            // (shouldn't be possible with () error type), shutdown
                            Ok(Async::Ready(None)) | Err(()) => {
                                state_changed = true;
                                break ExecutorState::Draining
                            },
                        }
                    }
                },
                ExecutorState::Draining => {
                    if self.transaction_counter.count() > 0 {
                        ExecutorState::Draining
                    } else {
                        return Ok(Async::Ready(()));
                    }
                },
                ExecutorState::Finished => panic!("Should not poll() after Executor is finished!"),
            };

            if !state_changed {
                break;
            }
        }

        Ok(Async::NotReady)
    }
}
