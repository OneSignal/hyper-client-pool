use std::fmt::Debug;

use crate::deliverable::Deliverable;

use hyper::body::Body;
use hyper_util::client::legacy::connect::dns::{GaiResolver, Name};
use hyper_util::client::legacy::connect::{Connect, HttpConnector};

use hyper_tls::HttpsConnector;
use std::sync::{Arc, RwLock};
use std::{marker::PhantomData, net::SocketAddr};
use tower_service::Service;

use super::Pool;
use crate::config::Config;
use crate::error::SpawnError;
use crate::executor::TransactionCounter;

pub type PoolConnector<R> = HttpsConnector<HttpConnector<R>>;

/// A trait used to wrap the PoolConnector used by default into a
/// different type that implements Connect
pub trait ConnectorAdaptor<R> {
    type Connect: Connect;

    fn wrap(connector: PoolConnector<R>) -> Self::Connect;
}

/// A trait used to create a DNS resolver. DefaultResolver uses the default DNS
/// resolver built into hyper.
pub trait CreateResolver {
    type Resolver: 'static
        + Clone
        + Send
        + Sync
        + Service<Name, Error = Self::Error, Future = Self::Future, Response = Self::Response>;

    type Error: 'static + Send + Sync + std::error::Error;
    type Future: Send + std::future::Future<Output = Result<Self::Response, Self::Error>>;
    type Response: Iterator<Item = SocketAddr>;

    fn create_resolver() -> Self::Resolver;
}

struct DefaultResolver;

impl CreateResolver for DefaultResolver {
    type Resolver = GaiResolver;

    type Error = <Self::Resolver as Service<Name>>::Error;
    type Future = <Self::Resolver as Service<Name>>::Future;
    type Response = <Self::Resolver as Service<Name>>::Response;

    fn create_resolver() -> Self::Resolver {
        GaiResolver::new()
    }
}

/// Default type that implemented ConnectorAdaptor, just passes through the connector
pub struct DefaultConnectorAdapator;

pub struct PoolBuilder<D: Deliverable, B: Body + Debug + Send + 'static>
where
    B::Data: Send,
{
    pub(in crate::pool) config: Config,
    pub(in crate::pool) transaction_counters: Option<Arc<RwLock<Vec<TransactionCounter>>>>,

    _d: PhantomData<D>,
    _b: PhantomData<B>,
}

impl<D: Deliverable, B: Body + Debug + Send + 'static> PoolBuilder<D, B>
where
    B::Data: Send,
    B::Error: std::error::Error + Send + Sync,
    B: Unpin,
{
    pub(in crate::pool) fn new(config: Config) -> PoolBuilder<D, B> {
        PoolBuilder {
            config,
            transaction_counters: None,

            _d: PhantomData,
            _b: PhantomData,
        }
    }

    pub fn build(self) -> Result<Pool<D, B>, SpawnError> {
        self.build_with_adaptor::<DefaultConnectorAdapator>()
    }

    /// Create the pool with a ConnectorAdaptor, a type that is used to
    /// wrap the hyper::Client's connector
    pub fn build_with_adaptor<A>(self) -> Result<Pool<D, B>, SpawnError>
    where
        A: ConnectorAdaptor<GaiResolver>,
        A::Connect: 'static + Clone + Send + Sync,
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: std::error::Error + Send + Sync,
        B: Unpin,
    {
        self.build_with_adaptor_and_resolver::<A, DefaultResolver>()
    }

    /// Create the pool with a ConnectorAdaptor, a type that is used to
    /// wrap the hyper::Client's connector
    pub fn build_with_adaptor_and_resolver<A, CR>(self) -> Result<Pool<D, B>, SpawnError>
    where
        A: ConnectorAdaptor<CR::Resolver>,
        A::Connect: 'static + Clone + Send + Sync,
        CR: CreateResolver,
        CR::Resolver: 'static + Clone + Send + Sync + Service<Name>,
        CR::Error: 'static + Send + Sync + std::error::Error,
        CR::Future: Send + std::future::Future<Output = Result<CR::Response, CR::Error>>,
        CR::Response: Iterator<Item = SocketAddr>,
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: std::error::Error + Send + Sync,
        B: Unpin,
    {
        Pool::new::<A, CR>(self)
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

impl<R> ConnectorAdaptor<R> for DefaultConnectorAdapator
where
    R: 'static + Clone + Send + Sync + Service<Name>,
    R::Error: 'static + Send + Sync + std::error::Error,
    R::Future: Send + std::future::Future<Output = Result<R::Response, R::Error>>,
    R::Response: Iterator<Item = SocketAddr>,
{
    type Connect = PoolConnector<R>;

    fn wrap(connector: PoolConnector<R>) -> Self::Connect {
        connector
    }
}
