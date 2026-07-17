mod config;
mod error;
mod health;
pub mod instance;
pub mod kind;
pub mod spec;
pub mod state;

pub use clash_api::Host;
pub use error::Error;
pub use kind::CoreKind;
pub use spec::{ControllerMode, CoreSpec, InstanceOptions, InstanceSpec, ManagerOptions, ResolvedController};
pub use state::{CoreState, CoreStatus, InstanceState, SpecSummary, StopReason};
