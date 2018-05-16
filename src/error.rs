use std::io;

use hyper_tls;

use deliverable::Deliverable;
use transaction::Transaction;

/// Error when spawning and configuring the thread that the [`hyper::Client`]s run on.
#[derive(Debug)]
pub enum SpawnError {
    ThreadSpawn(io::Error),
    HttpsConnector(hyper_tls::Error),
}

impl PartialEq for SpawnError {
    fn eq(&self, other: &SpawnError) -> bool {
        match self {
            SpawnError::ThreadSpawn(_err) => match other {
                SpawnError::ThreadSpawn(_oerr) => true,
                _ => false,
            },
            SpawnError::HttpsConnector(_err) => match other {
                SpawnError::HttpsConnector(_oerr) => true,
                _ => false,
            },
        }
    }
}

/// An error returned when requesting a Transaction.
#[derive(Debug)]
pub struct Error<D: Deliverable> {
    pub kind: ErrorKind,
    transaction: Transaction<D>,
}

impl<D: Deliverable> Error<D> {
    pub(crate) fn new(kind: ErrorKind, transaction: Transaction<D>) -> Error<D> {
        Error { kind, transaction }
    }

    pub fn into_transaction(self) -> Transaction<D> {
        self.transaction
    }
}

/// Types of errors that can occur when requesting.
/// A [`SpawnError`] can occur when requesting a Transaction as a new thread may need
/// spawned if a previous one was lost / invalidated.
#[derive(Debug, PartialEq)]
pub enum ErrorKind {
    /// An error occurred when spawning and configuring a new thread for a hyper::Client
    Spawn(SpawnError),
    /// There is no room for another transaction right now, the hyper::Client is full.
    Full,
}

/// Type of errors that can occur when attempting to send a [`Transaction`]
/// to an [`Executor`].
pub(crate) enum RequestError<D: Deliverable> {
    Full(Transaction<D>),
    FailedSend(Transaction<D>),
}
