//! The data engine behind dfox: the driver and session interfaces, query
//! results, connection models, and the engine that runs database work in the
//! background. Drivers for each database live in their own crates.

#[deny(missing_docs)]
pub mod arrow;
#[deny(missing_docs)]
pub mod catalog;
pub mod copy;
#[deny(missing_docs)]
pub mod driver;
pub mod engine;
pub mod errors;
pub mod export;
pub mod models;
pub mod result;
#[deny(missing_docs)]
pub mod session;
