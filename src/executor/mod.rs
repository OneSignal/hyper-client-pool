//! HTTP Client Worker Pool
//!
//! This module provides a simple API wrapping a pool of HTTP clients
use std::sync::Arc;
use std::time::Duration;

use futures::channel::mpsc as FuturesMpsc;
use futures::prelude::*;
use hyper::client::connect::{Connect, HttpConnector};
use hyper::{self, Client};
use hyper_tls::HttpsConnector;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::deliverable::Deliverable;
use crate::error::{RequestError, SpawnError};
use crate::pool::ConnectorAdaptor;
use crate::transaction::Transaction;
use raii_counter::{Counter, WeakCounter};

mod transaction_counter;

pub use self::transaction_counter::TransactionCounter;

/// Lives on a separate thread running a tokio_core::Reactor
/// and runs Transactions sent by the Pool.
pub(crate) struct Executor<D: Deliverable, C: 'static + Connect> {
    client: Arc<Client<C>>,
    transaction_counter: WeakCounter,
    transaction_timeout: Duration,
    receiver: FuturesMpsc::UnboundedReceiver<ExecutorMessage<D>>,
}

/// The handle to the Executor. It lives on the Pool thread
/// and allows message passing through futures::mpsc.
pub(crate) struct ExecutorHandle<D: Deliverable> {
    transaction_counter: WeakCounter,
    worker_counter: Counter,
    max_transactions: usize,

    sender: FuturesMpsc::UnboundedSender<ExecutorMessage<D>>,
    join_handle: JoinHandle<()>,
}

type ExecutorMessage<D> = (Transaction<D>, Counter);

impl<D: Deliverable> ExecutorHandle<D> {
    pub(crate) fn send(&mut self, transaction: Transaction<D>) -> Result<(), RequestError<D>> {
        if self.is_full() {
            return Err(RequestError::PoolFull(transaction));
        }

        let payload = (transaction, self.transaction_counter.spawn_upgrade());
        if let Err(err) = self.sender.unbounded_send(payload) {
            let (transaction, _counter) = err.into_inner();
            return Err(RequestError::FailedSend(transaction));
        }

        Ok(())
    }

    /// Shutdowns the executor by dropping the sender, returns the JoinHandle to the thread.
    pub(crate) fn shutdown(self) -> JoinHandle<()> {
        self.join_handle
    }

    pub(crate) fn transaction_counter(&self) -> TransactionCounter {
        TransactionCounter::new(
            WeakCounter::clone(&self.transaction_counter),
            Counter::clone(&self.worker_counter).downgrade(),
        )
    }

    fn is_full(&self) -> bool {
        self.transaction_counter.count() >= self.max_transactions
    }
}

impl<D: Deliverable, C: 'static + Connect + Clone + Send + Sync> Executor<D, C> {
    pub fn spawn<A, R>(config: &Config, resolver: R) -> Result<ExecutorHandle<D>, SpawnError>
    where
        A: ConnectorAdaptor<R, Connect = C>,
    {
        let (tx, rx) = FuturesMpsc::unbounded();
        let weak_counter = WeakCounter::new();
        let weak_counter_clone = weak_counter.clone();
        let keep_alive_timeout = config.keep_alive_timeout;
        let transaction_timeout = config.transaction_timeout.clone();

        let tls = tokio_tls::TlsConnector::from(native_tls::TlsConnector::new()?);

        let mut http = HttpConnector::new_with_resolver(resolver);
        http.enforce_http(false);
        // Set TCP_NODELAY to true to turn off Nagle's algorithm, an algorithm that
        // buffers sending / receiving data in packets which may be slowing down
        // our network traffic.
        //
        // See a relevant article: https://www.extrahop.com/company/blog/2016/tcp-nodelay-nagle-quickack-best-practices/
        http.set_nodelay(true);
        http.set_keepalive(Some(keep_alive_timeout));
        let connector = A::wrap(HttpsConnector::from((http, tls)));

        let client = Arc::new(
            hyper::Client::builder()
                .keep_alive(true)
                .keep_alive_timeout(Some(keep_alive_timeout))
                .build(connector),
        );

        let executor = Executor::<D, C> {
            receiver: rx,
            transaction_counter: weak_counter_clone,
            client,
            transaction_timeout,
        };

        let join_handle = tokio::spawn(executor.run());

        Ok(ExecutorHandle {
            transaction_counter: weak_counter,
            worker_counter: Counter::new(),
            max_transactions: config.max_transactions_per_worker,
            sender: tx,
            join_handle,
        })
    }

    async fn run(mut self) {
        while let Some((transaction, counter)) = self.receiver.next().await {
            trace!("Executor: spawning transaction.");

            transaction.spawn_request(
                Arc::clone(&self.client),
                self.transaction_timeout.clone(),
                counter,
            );
        }

        // We should only reach this point if the sender is closed and we stop
        // receiving transactions. This indicates a shutdown or error, which in
        // either case should cause a shutdown.
        while self.transaction_counter.count() > 0 {
            tokio::time::delay_for(self.transaction_timeout).await;
        }

        info!("Executor exited.");
    }
}
