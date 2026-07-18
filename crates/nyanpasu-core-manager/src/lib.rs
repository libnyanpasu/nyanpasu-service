//! Clash core lifecycle management: epoch-based instances, health-probed
//! startup, crash recovery, and core switching.
//!
//! Design: docs/superpowers/specs/2026-07-18-nyanpasu-core-manager-design.md

mod config;
mod error;
mod health;
pub mod instance;
pub mod kind;
pub mod manager;
pub mod spec;
pub mod state;

pub use clash_api::Host;
pub use error::Error;
pub use instance::Instance;
pub use kind::CoreKind;
pub use manager::CoreManager;
pub use spec::{
    ControllerMode, CoreSpec, InstanceOptions, InstanceSpec, ManagerOptions, ResolvedController,
};
pub use state::{CoreState, CoreStatus, InstanceState, SpecSummary, StopReason};
