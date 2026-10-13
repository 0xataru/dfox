//! The data engine behind dfox: the driver and session interfaces, query
//! results, connection models, and the engine that runs database work in the
//! background. Drivers for each database live in their own crates.

pub mod db;
pub mod driver;
pub mod engine;
pub mod errors;
pub mod models;
pub mod result;
