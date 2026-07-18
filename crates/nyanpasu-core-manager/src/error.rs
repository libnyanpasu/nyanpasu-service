use camino::Utf8PathBuf;

use crate::kind::CoreKind;

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
