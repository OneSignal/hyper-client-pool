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
