//! Clash core lifecycle management: epoch-based instances, health-probed
//! startup, crash recovery, and core switching.
//!
//! Design: docs/superpowers/specs/2026-07-18-nyanpasu-core-manager-design.md

mod error;
pub mod instance;
pub mod kind;

pub use clash_api::Host;
pub use error::Error;
pub use kind::CoreKind;
