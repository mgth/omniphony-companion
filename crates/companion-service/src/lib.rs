//! Shared application services. No UI toolkit or frontend event loop dependency.
pub mod alignment;
pub mod analysis_cache;
pub mod batch;
pub mod controller;
pub mod defaults;
pub mod duration;
pub mod files;
pub mod level_probe;
pub mod manual_level;
pub mod measurement;
pub mod microphone;
pub mod positions;
pub mod preferences;
pub mod review;
pub mod room;
pub mod room_response;
pub mod sweep_plan;
pub mod workspace;
pub type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

pub mod quality;
