use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};

use camino::Utf8PathBuf;
use enumset::EnumSet;
pub use nyanpasu_core_metadata::Feature;
use nyanpasu_core_metadata::{CoreVersion, FeatureSupport};

use crate::{Error, spec::CoreSpec};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct VersionCacheKey {
    binary_path: Utf8PathBuf,
    modified: SystemTime,
}

#[derive(Default)]
pub(crate) struct VersionCache {
    entries: tokio::sync::Mutex<HashMap<VersionCacheKey, String>>,
}

pub(crate) struct ResolvedFeatures {
    pub features: EnumSet<Feature>,
    pub version: Option<String>,
}

impl VersionCache {
    async fn resolve(&self, spec: &CoreSpec) -> Result<String, Error> {
        if let Some(version) = &spec.version {
            return Ok(version.clone());
        }
        let key = VersionCacheKey {
            binary_path: spec.binary_path.clone(),
            modified: binary_modified(spec).await?,
        };

        // Keep the lock across the short probe so concurrent resolutions in
        // one manager issue only one `-v` call for this binary.
        let mut entries = self.entries.lock().await;
        if let Some(version) = entries.get(&key) {
            return Ok(version.clone());
        }
        let version = probe_version(&spec.binary_path).await?;
        if binary_modified(spec).await? != key.modified {
            return Err(Error::CoreVersionProbeFailed {
                binary_path: spec.binary_path.clone(),
                detail:
                    "core binary changed during version probe; retry with the replacement binary"
                        .into(),
            });
        }
        entries.retain(|old, _| old.binary_path != key.binary_path);
        entries.insert(key, version.clone());
        Ok(version)
    }
}

async fn binary_modified(spec: &CoreSpec) -> Result<SystemTime, Error> {
    tokio::fs::metadata(&spec.binary_path)
        .await
        .map_err(|_| Error::BinaryNotFound(spec.binary_path.clone()))?
        .modified()
        .map_err(|error| Error::CoreVersionProbeFailed {
            binary_path: spec.binary_path.clone(),
            detail: error.to_string(),
        })
}

async fn probe_version(binary_path: &camino::Utf8Path) -> Result<String, Error> {
    let output = nyanpasu_utils::process::Command::new(binary_path.as_str())
        .arg("-v")
        .timeout(Duration::from_secs(5))
        .output()
        .await
        .map_err(|error| Error::CoreVersionProbeFailed {
            binary_path: binary_path.to_owned(),
            detail: error.to_string(),
        })?;
    if !output.success() {
        return Err(Error::CoreVersionProbeFailed {
            binary_path: binary_path.to_owned(),
            detail: format!("process exited with code {:?}", output.code),
        });
    }
    let stdout = output.stdout.trim();
    let stderr = output.stderr.trim();
    let version = if !stdout.is_empty() { stdout } else { stderr };
    if version.is_empty() {
        return Err(Error::CoreVersionProbeFailed {
            binary_path: binary_path.to_owned(),
            detail: "version command produced no output".into(),
        });
    }
    Ok(version.to_owned())
}

pub(crate) async fn resolve_features(
    cache: &VersionCache,
    core: &CoreSpec,
) -> Result<ResolvedFeatures, Error> {
    if core.kind.potential_features().is_empty() {
        return Ok(ResolvedFeatures {
            features: EnumSet::new(),
            version: core.version.clone(),
        });
    }
    let version = cache.resolve(core).await?;
    Ok(ResolvedFeatures {
        features: core.kind.features(Some(&CoreVersion::parse(&version))),
        version: Some(version),
    })
}
