//! Offline calibration policy over already-estimated observations.
//!
//! This crate neither captures audio nor estimates direct arrivals from PCM.
//! Hardware adapters must supply timing-referenced, comparable observations.
//! All public operations revalidate deserialized data before using it.

pub mod manual_level;
mod model;
mod policy;
pub mod repeatability;

pub use model::*;
pub use policy::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid calibration data: {0}")]
    Invalid(String),
    #[error("installation changed; measure again before applying or restoring this profile")]
    StaleInstallation,
    #[error("profile does not match this session and policy")]
    ProfileMismatch,
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}
