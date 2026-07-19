use camino::Utf8PathBuf;

use crate::{kind::CoreKind, state::RevisionId};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("core is already running")]
    AlreadyRunning,
    #[error("core is not running")]
    NotStarted,
    #[error("config file not found: {0}")]
    ConfigNotFound(Utf8PathBuf),
    #[error("core binary not found: {0}")]
    BinaryNotFound(Utf8PathBuf),
    #[error("no external controller configured; the version health probe needs one")]
    ControllerMissing,
    #[error("core kind `{0}` has no launch profile yet")]
    UnsupportedCore(CoreKind),
    #[error("config check failed: {0}")]
    ConfigCheckFailed(String),
    #[error("invalid runtime config: {0}")]
    InvalidConfig(String),
    #[error("invalid manager options: {0}")]
    InvalidManagerOptions(String),
    #[error("unsafe runtime artifact: {0}")]
    UnsafeRuntimeArtifact(Utf8PathBuf),
    #[error("runtime directory is already owned by another manager: {0}")]
    RuntimeDirectoryOwned(Utf8PathBuf),
    #[error("core process death could not be confirmed: {0}")]
    StopUnconfirmed(String),
    #[error("config revision conflict: expected {expected}, actual {actual:?}")]
    RevisionConflict {
        expected: RevisionId,
        actual: Option<RevisionId>,
    },
    #[error("config apply failed: {0}")]
    ApplyFailed(String),
    #[error("config apply failed ({apply}); rollback also failed ({rollback})")]
    ApplyRollbackFailed { apply: String, rollback: String },
    #[error("core did not become healthy before the startup timeout; stderr tail:\n{stderr_tail}")]
    StartupTimeout { stderr_tail: String },
    #[error("core failed to start; stderr tail:\n{stderr_tail}")]
    StartupFailed { stderr_tail: String },
    #[error(transparent)]
    Process(#[from] nyanpasu_utils::process::ProcessError),
    #[error(transparent)]
    Api(#[from] clash_api::Error),
    #[error("failed to process config YAML: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
