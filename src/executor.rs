//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::mem;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures::{Poll, Future, Stream, Async};
use futures::sync::mpsc as FuturesMpsc;
use hyper_tls::HttpsConnector;
use hyper::{self, Client};
use hyper::client::HttpConnector;
use native_tls::TlsConnector;
use tokio_core::reactor::{Core, Handle};

use config::Config;
use deliverable::Deliverable;
use error::{RequestError, SpawnError};
use raii_counter::{Counter, WeakCounter};
use transaction::Transaction;

/// Lives on a separate thread and runs Transactions sent by Pool
pub struct Executor<D: Deliverable> {
    handle: Handle,
    client: Client<HttpsConnector<HttpConnector>>,
    transaction_counter: WeakCounter,
    transaction_timeout: Duration,
    state: ExecutorState<D>,
}

pub struct ExecutorHandle<D: Deliverable> {
    transaction_counter: WeakCounter,
    max_transactions: usize,

    sender: FuturesMpsc::UnboundedSender<ExecutorMessage<D>>,
    join_handle: JoinHandle<()>,
}

enum ExecutorState<D: Deliverable> {
    Running(FuturesMpsc::UnboundedReceiver<ExecutorMessage<D>>),
    Draining,
    Finished,
}

enum ExecutorMessage<D: Deliverable> {
    Transaction((Transaction<D>, Counter)),
    Shutdown,
}

impl<D: Deliverable> ExecutorHandle<D> {
    pub fn send(&mut self, transaction: Transaction<D>) -> Result<(), RequestError<D>> {
        if self.is_full() {
            return Err(RequestError::Full(transaction));
        }

        let package = ExecutorMessage::Transaction((transaction, self.transaction_counter.spawn_upgrade()));
        if let Err(err) = self.sender.unbounded_send(package) {
            match err.into_inner() {
                ExecutorMessage::Transaction((transaction, _counter)) => {
                    return Err(RequestError::FailedSend(transaction));
                },
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    pub fn send_shutdown(self) -> JoinHandle<()> {
        let _ = self.sender.unbounded_send(ExecutorMessage::Shutdown);
        self.join_handle
    }

    fn is_full(&self) -> bool {
        self.transaction_counter.count() >= self.max_transactions
    }
}

impl<D: Deliverable> Executor<D> {
    pub fn spawn(config: &Config) -> Result<ExecutorHandle<D>, SpawnError> {
        let (tx, rx) = FuturesMpsc::unbounded();
        let weak_counter = WeakCounter::new();
        let weak_counter_clone = weak_counter.clone();
        let keep_alive_timeout = config.keep_alive_timeout;
        let dns_threads_per_worker = config.dns_threads_per_worker;
        let transaction_timeout = config.transaction_timeout.clone();

        info!("Spawning Executor.");
        let tls = TlsConnector::builder().and_then(|builder| builder.build()).map_err(SpawnError::HttpsConnector)?;

        thread::Builder::new()
            .name(format!("Hyper-Client-Pool Executor"))
            .spawn(move || {
                let mut core = Core::new().unwrap();
                let handle = core.handle();

                let mut http = HttpConnector::new(dns_threads_per_worker, &handle);
                http.enforce_http(false);
                let connector = HttpsConnector::from((http, tls));
                let client = hyper::Client::configure()
                    .connector(connector)
                    .keep_alive(true)
                    .keep_alive_timeout(Some(keep_alive_timeout))
                    .build(&handle);

                let executor = Executor {
                    state: ExecutorState::Running(rx),
                    handle: handle,
                    transaction_counter: weak_counter_clone,
                    client,
                    transaction_timeout,
                };

                if let Err(err) = core.run(executor) {
                    warn!("Error when running Executor: {:?}", err);
                }

                info!("Executor exited.");
            })
            .map(|join_handle| {
                ExecutorHandle {
                    transaction_counter: weak_counter,
                    max_transactions: config.max_transactions_per_worker,
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
                            Ok(Async::Ready(Some(msg))) => {
                                match msg {
                                    ExecutorMessage::Transaction((transaction, counter)) => {
                                        info!("Executor: spawning transaction.");

                                        transaction.spawn_request(
                                            &self.client,
                                            &self.handle,
                                            self.transaction_timeout.clone(),
                                            counter
                                        );
                                    },
                                    ExecutorMessage::Shutdown => {
                                        info!("Executor: received shutdown notice, shutting down..");
                                        // There cannot be any other messages in the receiver queue
                                        // because sending the shutdown drops the receiver
                                        state_changed = true;
                                        break ExecutorState::Draining;
                                    },
                                };
                            },
                            // No messages
                            Ok(Async::NotReady) => break ExecutorState::Running(receiver),
                            // All senders dropped or errored
                            // (shouldn't be possible with () error type), shutdown
                            Ok(Async::Ready(None)) | Err(()) => break ExecutorState::Draining,
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
