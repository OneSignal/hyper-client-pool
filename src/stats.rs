use antidote::RwLock;

pub trait StatsDelegate: 'static + Send + Sync {
    fn handle_response_size_received(&self, response_size: usize);
}

lazy_static! {
    pub(crate) static ref STATS_DELEGATES: RwLock<Vec<Box<StatsDelegate>>> =
        RwLock::new(Vec::new());
}

pub fn register_delegate<S: 'static + StatsDelegate>(delegate: S) {
    STATS_DELEGATES.write().push(Box::new(delegate))
}

pub(crate) fn response_size_received(response_size: usize) {
    for delegate in STATS_DELEGATES.read().iter() {
        delegate.handle_response_size_received(response_size);
    }
}
