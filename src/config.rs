use std::time::Duration;

#[derive(Clone)]
pub struct Config {
    /// How long to keep a connection alive before timing out
    pub keep_alive_timeout: Duration,

    /// Transaction timeout (in seconds)
    pub transaction_timeout: Duration,

    /// Max transactions per worker spawned
    pub max_transactions_per_worker: usize,

    /// Number of workers in the pool
    pub workers: usize,

    /// Number of DNS threads per worker
    pub dns_threads_per_worker: usize,
}

#[cfg(test)]
impl Default for Config {
    fn default() -> Config {
        Config {
            keep_alive_timeout: Duration::from_secs(300),
            transaction_timeout: Duration::from_secs(10),
            max_transactions_per_worker: 1_000,
            workers: 2,
            dns_threads_per_worker: 10,
        }
    }
}
