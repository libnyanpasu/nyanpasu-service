use std::{borrow::Cow, ffi::OsStr, path::Path};

#[derive(Debug, Clone)]
pub enum ClashCoreType {
    Mihomo,
    MihomoAlpha,
    ClashRust,
    ClashPremium,
}

impl ClashCoreType {
    pub(super) fn get_run_args<'a, P: Into<Cow<'a, Path>>>(
        &self,
        app_dir: P,
        config_path: P,
    ) -> Vec<Cow<'a, OsStr>> {
        let app_dir: Cow<'a, Path> = app_dir.into();
        let config_path: Cow<'a, Path> = config_path.into();
        let app_dir: Cow<'a, OsStr> = Cow::Owned(app_dir.as_ref().as_os_str().to_owned());
        let config_path: Cow<'a, OsStr> = Cow::Owned(config_path.as_ref().as_os_str().to_owned());
        match self {
            ClashCoreType::Mihomo | ClashCoreType::MihomoAlpha => vec![
                Cow::Borrowed(OsStr::new("-m")),
                Cow::Borrowed(OsStr::new("-d")),
                app_dir,
                Cow::Borrowed(OsStr::new("-f")),
                config_path,
            ],
            ClashCoreType::ClashRust => {
                vec![
                    Cow::Borrowed(OsStr::new("-d")),
                    app_dir,
                    Cow::Borrowed(OsStr::new("-c")),
                    config_path,
                ]
            }
            ClashCoreType::ClashPremium => {
                vec![
                    Cow::Borrowed(OsStr::new("-d")),
                    app_dir,
                    Cow::Borrowed(OsStr::new("-f")),
                    config_path,
                ]
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum CoreType {
    Clash(ClashCoreType),
    SingBox, // Maybe we would support this in the 2.x?
}

pub struct TerminatedPayload {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

pub enum CommandEvent {
    Stdout(String),
    Stderr(String),
    Error(String),
    Terminated(TerminatedPayload),
}
