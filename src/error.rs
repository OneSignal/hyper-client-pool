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
pub enum NewError {
    ThreadSpawn(io::Error),
    HttpsConnector(hyper_tls::Error),
}

#[derive(Debug)]
pub enum RequestError<D: Deliverable> {
    Full(Transaction<D>),
    FailedSend(Transaction<D>),
}

impl NewError {
    pub fn convert<D: Deliverable>(error: Error<D>) -> NewError {
        match error {
            Error::ThreadSpawn(err) => NewError::ThreadSpawn(err),
            Error::HttpsConnector(err) => NewError::HttpsConnector(err),
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
