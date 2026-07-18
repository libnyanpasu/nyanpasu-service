//! Core kinds, launch profiles, and config checking.

use std::ffi::OsString;

use camino::Utf8Path;

use crate::error::Error;

/// The environment variable Mihomo consults for permitted file-system roots.
pub const MIHOMO_SAFE_PATHS_ENV_NAME: &str = "SAFE_PATHS";

#[cfg(windows)]
const SAFE_PATHS_SEPARATOR: &str = ";";
#[cfg(not(windows))]
const SAFE_PATHS_SEPARATOR: &str = ":";

/// A core family. Build variants (alpha builds, custom binaries) are expressed
/// through `CoreSpec::binary_path` and metadata, not extra kinds.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoreKind {
    Mihomo,
    ClashPremium,
    ClashRs,
    /// Declared for a future core; has no launch profile yet.
    Meow,
}

impl AsRef<str> for CoreKind {
    fn as_ref(&self) -> &str {
        match self {
            CoreKind::Mihomo => "mihomo",
            CoreKind::ClashPremium => "clash",
            CoreKind::ClashRs => "clash-rs",
            CoreKind::Meow => "meow",
        }
    }
}

impl std::fmt::Display for CoreKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_ref())
    }
}

impl CoreKind {
    /// Launch arguments for this kind. `Meow` has no launch profile yet.
    pub(crate) fn run_args(
        &self,
        working_dir: &Utf8Path,
        config_path: &Utf8Path,
    ) -> Result<Vec<OsString>, Error> {
        let dir = OsString::from(working_dir.as_str());
        let cfg = OsString::from(config_path.as_str());
        Ok(match self {
            CoreKind::Mihomo => vec!["-m".into(), "-d".into(), dir, "-f".into(), cfg],
            CoreKind::ClashRs => vec!["-d".into(), dir, "-c".into(), cfg],
            CoreKind::ClashPremium => vec!["-d".into(), dir, "-f".into(), cfg],
            CoreKind::Meow => return Err(Error::UnsupportedCore(*self)),
        })
    }

    /// Arguments for a one-shot `-t` config validation run (same for all kinds,
    /// matching the legacy `check_config_`).
    pub(crate) fn check_args(working_dir: &Utf8Path, config_path: &Utf8Path) -> Vec<OsString> {
        vec![
            "-t".into(),
            "-d".into(),
            working_dir.as_str().into(),
            "-f".into(),
            config_path.as_str().into(),
        ]
    }
}

/// Joins the directories Mihomo may touch into its `SAFE_PATHS` format.
pub fn mihomo_safe_paths(working_dir: &Utf8Path, config_dir: &Utf8Path) -> String {
    [working_dir.as_str(), config_dir.as_str()].join(SAFE_PATHS_SEPARATOR)
}

/// Extracts the human-readable message from a Mihomo error log line.
/// Behavioral port of the legacy `core::utils::parse_check_output`.
pub(crate) fn parse_check_output(log: String) -> String {
    let t = log.find("time=");
    let m = log.find("msg=");
    let mr = log.rfind('"');

    if let (Some(_), Some(m), Some(mr)) = (t, m, mr) {
        let e = match log.find("level=error msg=") {
            Some(e) => e + 17,
            None => m + 5,
        };

        if mr > m {
            return log[e..mr].to_owned();
        }
    }

    let l = log.find("error=");
    let r = log.find("path=").or(Some(log.len()));

    if let (Some(l), Some(r)) = (l, r) {
        return log[(l + 6)..(r - 1)].to_owned();
    }

    log
}

/// Condenses a stderr tail into an error message. Mihomo logs are structured,
/// so the last `level=error` line carries the actual cause.
pub(crate) fn error_summary(kind: CoreKind, stderr_tail: &str) -> String {
    if matches!(kind, CoreKind::Mihomo)
        && let Some(line) = stderr_tail
            .lines()
            .rev()
            .find(|l| l.contains("level=error"))
    {
        return parse_check_output(line.to_string());
    }
    stderr_tail.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn run_args_match_legacy_profiles() {
        let dir = Utf8PathBuf::from("C:/data");
        let cfg = Utf8PathBuf::from("C:/data/config.yaml");
        let args = CoreKind::Mihomo.run_args(&dir, &cfg).unwrap();
        assert_eq!(
            args,
            ["-m", "-d", "C:/data", "-f", "C:/data/config.yaml"].map(OsString::from)
        );
        let args = CoreKind::ClashRs.run_args(&dir, &cfg).unwrap();
        assert_eq!(
            args,
            ["-d", "C:/data", "-c", "C:/data/config.yaml"].map(OsString::from)
        );
        let args = CoreKind::ClashPremium.run_args(&dir, &cfg).unwrap();
        assert_eq!(
            args,
            ["-d", "C:/data", "-f", "C:/data/config.yaml"].map(OsString::from)
        );
    }

    #[test]
    fn meow_has_no_launch_profile() {
        let dir = Utf8PathBuf::from("/d");
        assert!(matches!(
            CoreKind::Meow.run_args(&dir, &dir),
            Err(Error::UnsupportedCore(CoreKind::Meow))
        ));
    }

    #[test]
    fn safe_paths_joins_with_platform_separator() {
        let joined = mihomo_safe_paths(Utf8Path::new("/a"), Utf8Path::new("/b"));
        #[cfg(windows)]
        assert_eq!(joined, "/a;/b");
        #[cfg(not(windows))]
        assert_eq!(joined, "/a:/b");
    }

    #[test]
    fn parse_check_output_extracts_mihomo_msg() {
        let log = r#"time="2026-07-18T10:00:00Z" level=error msg="configuration file /x.yaml test failed""#;
        assert_eq!(
            parse_check_output(log.to_string()),
            "configuration file /x.yaml test failed"
        );
    }

    #[test]
    fn parse_check_output_extracts_error_field() {
        assert_eq!(parse_check_output("error=bad path=/etc".to_string()), "bad");
    }

    #[test]
    fn parse_check_output_falls_back_to_input() {
        assert_eq!(
            parse_check_output("plain failure".to_string()),
            "plain failure"
        );
    }

    #[test]
    fn error_summary_parses_last_mihomo_error_line() {
        let tail = "line one\ntime=\"x\" level=error msg=\"boom\"\nafter";
        assert_eq!(error_summary(CoreKind::Mihomo, tail), "boom");
        assert_eq!(error_summary(CoreKind::ClashRs, tail), tail);
        assert_eq!(error_summary(CoreKind::Mihomo, "no marker"), "no marker");
    }
}
