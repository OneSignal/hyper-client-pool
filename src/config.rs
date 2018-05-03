pub struct Config {
    /// How long to keep a connection alive before timing out
    pub keep_alive_timeout: Duration,

    /// Connection timeout (in seconds)
    pub connection_timeout: Duration,

    /// Maximum sockets per worker
    pub max_sockets: usize,

    /// Maximum concurrent transactions
    ///
    /// This warrants some explanation since it might be assumed to always be
    /// the same as max sockets. Indeed, if not specified, it will have the
    /// value of max_sockets. This can be useful to provide a buffer of socket
    /// space when using keep alive so that transactions on a new domain won't
    /// necessarily cause keep-alive sockets to be closed. Let's just use an
    /// example.
    ///
    /// Say there's 100 max sockets and 50 max transactions. Now let's say 50
    /// transactions are started at once to google.com, and keep-alive is being
    /// used. Now, there's 50 sockets in use.  Those transactions finish, and 50
    /// requests are made to apple.com. If max_sockets matched max_transactions,
    /// all of those sockets in keep-alive would be closed and reopened for the
    /// new host. By making max_sockets larger than max_transactions, there's
    /// extra space available for stuff in keep-alive to prevent rapid
    /// disconnecting and reconnecting.
    ///
    /// Would be nice to have a more succinct explanation for this.
    pub max_transactions: Option<usize>,

    /// Number of workers in the pool
    pub workers: usize,

    /// Number of DNS threads per worker
    pub dns_threads_per_worker: usize,
}

impl Config {
    /// Get the maximum number of transactions
    ///
    /// If this value is larger than max_sockets, the max_sockets value is
    /// returned.
    pub fn max_transactions(&self) -> usize {
        self.max_transactions
            .map(|trans| cmp::min(trans, self.max_sockets))
            .unwrap_or(self.max_sockets)
    }
}

impl Default for Config {
    fn default() -> Config {
        Config {
            keep_alive_timeout: Duration::from_secs(300),
            connection_timeout: Duration::from_secs(10),
            max_sockets: 1_000,
            max_transactions: None,
            workers: 2,
            dns_threads_per_worker: 10,
        }
    }
}
