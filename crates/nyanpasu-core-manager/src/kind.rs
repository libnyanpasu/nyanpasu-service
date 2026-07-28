//! Core kinds, launch profiles, and config checking.

use std::ffi::OsString;

use camino::Utf8Path;

use crate::error::Error;

pub use nyanpasu_core_metadata::ClashCoreKind as CoreKind;

/// The environment variable Mihomo consults for permitted file-system roots.
pub const MIHOMO_SAFE_PATHS_ENV_NAME: &str = "SAFE_PATHS";

#[cfg(windows)]
const SAFE_PATHS_SEPARATOR: &str = ";";
#[cfg(not(windows))]
const SAFE_PATHS_SEPARATOR: &str = ":";

/// Launch arguments for this kind. `Meow` has no launch profile yet.
pub(crate) fn run_args(
    kind: CoreKind,
    working_dir: &Utf8Path,
    config_path: &Utf8Path,
) -> Result<Vec<OsString>, Error> {
    let dir = OsString::from(working_dir.as_str());
    let cfg = OsString::from(config_path.as_str());
    Ok(match kind {
        CoreKind::Mihomo => vec!["-m".into(), "-d".into(), dir, "-f".into(), cfg],
        CoreKind::ClashRust => vec!["-d".into(), dir, "-c".into(), cfg],
        CoreKind::ClashPremium => vec!["-d".into(), dir, "-f".into(), cfg],
        CoreKind::Meow => return Err(Error::UnsupportedCore(kind)),
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

/// Joins the directories Mihomo may touch into its `SAFE_PATHS` format.
pub fn mihomo_safe_paths(working_dir: &Utf8Path, config_dir: &Utf8Path) -> String {
    [working_dir.as_str(), config_dir.as_str()].join(SAFE_PATHS_SEPARATOR)
}

/// One-shot `-t` config validation, replacing the legacy `check_config_`.
/// A non-zero exit becomes [`Error::ConfigCheckFailed`] with a condensed message.
pub async fn check_config(spec: &crate::spec::InstanceSpec) -> Result<(), Error> {
    if matches!(spec.core.kind, CoreKind::Meow) {
        return Err(Error::UnsupportedCore(spec.core.kind));
    }
    let config_dir = spec
        .config_path
        .parent()
        .ok_or_else(|| Error::ConfigNotFound(spec.config_path.clone()))?;
    let output = nyanpasu_utils::process::Command::new(spec.core.binary_path.as_str())
        .args(check_args(&spec.working_dir, &spec.config_path))
        .env(
            MIHOMO_SAFE_PATHS_ENV_NAME,
            mihomo_safe_paths(&spec.working_dir, config_dir),
        )
        .output()
        .await?;
    if output.success() {
        return Ok(());
    }
    let message = match spec.core.kind {
        CoreKind::ClashRust => format!("{}\n{}", output.stdout, output.stderr),
        _ => parse_check_output(output.stdout.trim().to_owned()),
    };
    Err(Error::ConfigCheckFailed(message))
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
    let r = log.find("path=").unwrap_or(log.len());

    if let Some(l) = l {
        let start = l + 6;
        if r >= start {
            return log[start..r].trim_end().to_owned();
        }
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
        let args = run_args(CoreKind::Mihomo, &dir, &cfg).unwrap();
        assert_eq!(
            args,
            ["-m", "-d", "C:/data", "-f", "C:/data/config.yaml"].map(OsString::from)
        );
        let args = run_args(CoreKind::ClashRust, &dir, &cfg).unwrap();
        assert_eq!(
            args,
            ["-d", "C:/data", "-c", "C:/data/config.yaml"].map(OsString::from)
        );
        let args = run_args(CoreKind::ClashPremium, &dir, &cfg).unwrap();
        assert_eq!(
            args,
            ["-d", "C:/data", "-f", "C:/data/config.yaml"].map(OsString::from)
        );
    }

    #[test]
    fn meow_has_no_launch_profile() {
        let dir = Utf8PathBuf::from("/d");
        assert!(matches!(
            run_args(CoreKind::Meow, &dir, &dir),
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
    fn parse_check_output_handles_fallback_error_boundaries() {
        assert_eq!(parse_check_output("error=bad".to_string()), "bad");
        assert_eq!(
            parse_check_output("path=/etc error=bad".to_string()),
            "path=/etc error=bad"
        );
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
        assert_eq!(error_summary(CoreKind::ClashRust, tail), tail);
        assert_eq!(error_summary(CoreKind::Mihomo, "no marker"), "no marker");
    }
}
