use deliverable::Deliverable;
use std::marker::PhantomData;
use std::sync::{Arc, RwLock};

use super::Pool;
use config::Config;
use error::SpawnError;
use executor::TransactionCounter;

pub struct PoolBuilder<D: Deliverable> {
    pub(in pool) config: Config,
    pub(in pool) transaction_counters: Option<Arc<RwLock<Vec<TransactionCounter>>>>,

    _d: PhantomData<D>,
}

impl<D: Deliverable> PoolBuilder<D> {
    pub(in pool) fn new(config: Config) -> PoolBuilder<D> {
        PoolBuilder {
            config,
            transaction_counters: None,

            _d: PhantomData,
        }
    }

    pub fn build(self) -> Result<Pool<D>, SpawnError> {
        Pool::new(self)
    }

    /// Pass in an synchronized Vec<Weak<WeakCounter>> that will be populated
    /// with transaction counters for each of the workers spawned.
    ///
    /// You can check that the WeakCounter is still valid by ensuring that the
    /// Arc::strong_count on the Weak reference
    pub fn transaction_counters(mut self, value: Arc<RwLock<Vec<TransactionCounter>>>) -> Self {
        self.transaction_counters = Some(value);
        self
    }
}
