//! PostgreSQL adapter for authoritative `w9pt` filesystem state.
//!
//! This crate is under active unreleased development. Earlier development
//! schemas have no upgrade path and must be recreated after an incompatible
//! schema change.

#![forbid(unsafe_code)]

mod change;
mod clock;
mod commit;
mod config;
mod database;
mod error;
mod key_codec;
mod lease;
mod migration;
mod numeric;
mod read;
mod record_write;
mod row_codec;
mod schema;
mod sqlstate;
mod store;
mod transaction;
mod validation;

#[cfg(feature = "test-support")]
pub mod testing;

pub use clock::{LEASE_TICK_UNIT_MICROSECONDS, LEASE_TICKS_PER_SECOND, LeaseClockSource};
pub use config::{PostgresConfigError, PostgresStateConfig, PrimaryWalDurability, SchemaLimit};
pub use error::{MigrationError, PostgresOpenError, PostgresStateError};
pub use migration::PRODUCTION_SCHEMA;
pub use store::{MigrationReport, PostgresStateStore};
