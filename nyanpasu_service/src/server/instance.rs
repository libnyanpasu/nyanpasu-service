use nyanpasu_utils::core::instance::CoreInstance;
use parking_lot::RwLock;
use std::{
    borrow::Cow,
    path::PathBuf,
    string,
    sync::{Arc, OnceLock},
};

struct CoreManager {
    instance: Arc<RwLock<CoreInstance>>,
    config_path: PathBuf,
}

type StateChangedAt = i64;

pub struct CoreManagerWrapper(Option<CoreManager>, StateChangedAt);
impl CoreManagerWrapper {
    pub fn global() -> &'static Arc<RwLock<CoreManagerWrapper>> {
        static INSTANCE: OnceLock<Arc<RwLock<CoreManagerWrapper>>> = OnceLock::new();
        INSTANCE.get_or_init(|| Arc::new(RwLock::new(CoreManagerWrapper(None, 0))))
    }

    pub fn status(&self) -> nyanpasu_ipc::api::status::CoreInfos {
        match self.0 {
            None => nyanpasu_ipc::api::status::CoreInfos {
                r#type: None,
                state: nyanpasu_ipc::api::status::CoreState::Stopped(None),
                state_changed_at: self.1,
                config_path: None,
            },
            Some(ref manager) => {
                let instance = manager.instance.read();
                nyanpasu_ipc::api::status::CoreInfos {
                    r#type: Some(instance.core_type.clone()),
                    state: match instance.state() {
                        nyanpasu_utils::core::instance::CoreInstanceState::Running => {
                            nyanpasu_ipc::api::status::CoreState::Running
                        }
                        nyanpasu_utils::core::instance::CoreInstanceState::Stopped => {
                            nyanpasu_ipc::api::status::CoreState::Stopped(None)
                        }
                    },
                    state_changed_at: self.1,
                    config_path: Some(manager.config_path.clone()),
                }
            }
        }
    }
}
