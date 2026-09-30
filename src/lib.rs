mod config;
mod deliverable;
mod error;
mod executor;
mod pool;
mod transaction;

pub use config::Config;
pub use deliverable::Deliverable;
pub use error::{Error, ErrorKind, SpawnError};
pub use executor::TransactionCounter;
pub use pool::{
    ConnectorAdaptor, CreateResolver, DefaultConnectorAdapator, Pool, PoolBuilder, PoolConnector,
};
pub use transaction::{DeliveryResult, Transaction};

pub type EmptyBody = http_body_util::Empty<bytes::Bytes>;
pub type Body = http_body_util::Full<bytes::Bytes>;
pub type HyperClientPoolError = Box<dyn std::error::Error + Send + Sync>;
