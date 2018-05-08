use std::io;

use hyper_tls;

use dispatcher::Dispatcher;
use transaction::Transaction;

#[derive(Debug)]
pub enum Error<D: Dispatcher> {
    ThreadSpawn(io::Error),
    HttpsConnector(hyper_tls::Error),
    Full(Transaction<D>),
    FailedSend(Transaction<D>),
}
