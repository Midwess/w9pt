//! Amazon S3 implementation of the backend-neutral target-object contract.
//!
//! This adapter accepts a caller-created AWS SDK S3 client. The embedding
//! application retains ownership of credentials, region, endpoint, TLS, retry
//! and timeout configuration, and the Tokio runtime. Repository object keys are
//! forwarded verbatim; filesystem paths and authoritative metadata never enter
//! this crate.
//!
//! This crate is under active unreleased development. Its API and adapter token
//! formats may change without compatibility support for earlier builds.

#![forbid(unsafe_code)]

mod body;
mod classify;
mod config;
mod error;
mod qualification;
mod range;
mod store;
mod version;

pub use config::{
    BodyTimeout, S3ProviderProfile, S3QualificationNamespace, S3TargetConfig, S3TargetConfigBuilder,
};
pub use error::{S3ConfigurationError, S3Error, S3Operation, S3RequestIds};
pub use store::S3Target;
