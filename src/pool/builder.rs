use deliverable::Deliverable;
use hyper::client::connect::Connect;
use hyper_http_connector::HttpConnector;
use hyper_tls::HttpsConnector;
use std::marker::PhantomData;
use std::sync::{Arc, RwLock};

use super::Pool;
use config::Config;
use error::SpawnError;
use executor::TransactionCounter;

pub type PoolConnector = HttpsConnector<HttpConnector>;

/// A trait used to wrap the PoolConnector used by default into a
/// different type that implements Connect
pub trait ConnectorAdaptor {
    type Connect: Connect;

    fn wrap(connector: PoolConnector) -> Self::Connect;
}

/// Default type that implemented ConnectorAdaptor, just passes through the connector
pub struct DefaultConnectorAdapator;

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
        Pool::new::<DefaultConnectorAdapator>(self)
    }

    /// Create the pool with a ConnectorAdaptor, a type that is used to
    /// wrap the hyper::Client's connector
    pub fn build_with_adaptor<C>(self) -> Result<Pool<D>, SpawnError>
    where
        C: ConnectorAdaptor,
        C::Connect: 'static,
    {
        Pool::new::<C>(self)
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

impl ConnectorAdaptor for DefaultConnectorAdapator {
    type Connect = PoolConnector;

    fn wrap(connector: PoolConnector) -> Self::Connect {
        connector
    }
}
