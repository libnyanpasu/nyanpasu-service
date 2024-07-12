use std::{future::Future, sync::OnceLock};

use tokio::runtime::Runtime;

pub static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn default_runtime() -> Runtime {
    let runtime = Runtime::new().unwrap();
    runtime
}

/// Runs a future to completion on runtime.
pub fn block_on<F: Future>(task: F) -> F::Output {
    let runtime = RUNTIME.get_or_init(default_runtime);
    runtime.block_on(task)
}
