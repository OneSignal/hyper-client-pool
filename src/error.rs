use std::io;

use hyper_tls;

use deliverable::Deliverable;
use transaction::Transaction;

#[derive(Debug)]
pub enum Error<D: Deliverable> {
    ThreadSpawn(io::Error),
    HttpsConnector(hyper_tls::Error),
    Full(Transaction<D>),
    FailedSend(Transaction<D>),
}

#[derive(Debug)]
pub enum SpawnError {
    ThreadSpawn(io::Error),
    HttpsConnector(hyper_tls::Error),
}

#[derive(Debug)]
pub enum RequestError<D: Deliverable> {
    Full(Transaction<D>),
    FailedSend(Transaction<D>),
}

impl SpawnError {
    pub fn convert<D: Deliverable>(error: Error<D>) -> SpawnError {
        match error {
            Error::ThreadSpawn(err) => SpawnError::ThreadSpawn(err),
            Error::HttpsConnector(err) => SpawnError::HttpsConnector(err),
            _ => unreachable!(),
        }
    }
}

impl<D: Deliverable> RequestError<D> {
    pub fn convert(error: Error<D>) -> RequestError<D> {
        match error {
            Error::Full(err) => RequestError::Full(err),
            Error::FailedSend(err) => RequestError::FailedSend(err),
            _ => unreachable!(),
        }
    }
}
