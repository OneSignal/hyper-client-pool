use std::time::Duration;

#[derive(Clone)]
/// Configuration for [`pool::Pool`].
pub struct Config {
    /// How long to keep a connection alive before timing out
    pub keep_alive_timeout: Duration,

    /// Transaction timeout (in seconds)
    pub transaction_timeout: Duration,

    /// Number of DNS threads per worker
    pub dns_threads_per_worker: usize,

    /// Max idle connections per worker, trying to create
    /// requests past this limit will wait for an idle connection
    /// or fail if no idle connection can be created
    pub max_connections_per_worker: usize,

    /// Max transactions per worker spawned
    pub max_transactions_per_worker: usize,

    /// Number of workers in the pool
    pub workers: usize,
}
