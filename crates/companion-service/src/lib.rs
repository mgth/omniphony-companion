//! Shared application services. No UI toolkit or frontend event loop dependency.
pub mod batch;
pub mod controller;
pub mod files;
pub mod measurement;
pub mod review;
pub type AppResult<T> = Result<T, Box<dyn std::error::Error>>;
