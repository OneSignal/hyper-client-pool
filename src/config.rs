use std::time::Duration;

#[derive(Clone)]
/// Configuration for [`pool::Pool`].
pub struct Config {
    /// How long to keep a connection alive before timing out
    pub keep_alive_timeout: Duration,

    /// Transaction timeout (in seconds)
    pub transaction_timeout: Duration,

    /// Max transactions per worker spawned
    pub max_transactions_per_worker: usize,

    /// Number of workers in the pool
    pub workers: usize,
}
