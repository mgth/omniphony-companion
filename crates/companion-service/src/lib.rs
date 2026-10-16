//! Shared application services. No UI toolkit or frontend event loop dependency.
pub mod alignment;
pub mod batch;
pub mod controller;
pub mod files;
pub mod measurement;
pub mod microphone;
pub mod preferences;
pub mod review;
pub mod workspace;
pub type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

pub mod quality;
