//! Clash core lifecycle management: epoch-based instances, health-probed
//! startup, crash recovery, and core switching.
//!
//! Design: docs/superpowers/specs/2026-07-18-nyanpasu-core-manager-design.md

mod config;
mod config_diff;
mod error;
mod health;
pub mod instance;
pub mod kind;
pub mod manager;
pub mod probe;
mod probe_driver;
pub mod runtime_store;
pub mod spec;
pub mod state;

pub use clash_api::Host;
pub use error::Error;
pub use health::HealthPolicy;
pub use instance::{Instance, InstanceBuilder};
pub use kind::CoreKind;
pub use manager::{ApplyOutcome, CoreManager, CoreManagerBuilder, DegradeReason, SwitchOutcome};
pub use probe::{
    ControllerVersionProbe, HealthProbe, ProbeContext, ProbeFuture, ProbeHandle, ProbePhase,
    ProbeResult,
};
pub use runtime_store::{
    RuntimeCommitDurability, RuntimeConfigBackup, RuntimeConfigCommit, RuntimeConfigStore,
    StagedRuntimeConfig,
};
pub use spec::{
    ControllerMode, CoreSpec, InstanceOptions, InstanceSpec, ManagerOptions, ResolvedController,
};
pub use state::{
    ConfigRevision, CoreState, CoreStatus, HealthState, HealthStatus, InstanceState,
    InstanceStatus, RevisionId, SpecSummary, StopReason,
};
