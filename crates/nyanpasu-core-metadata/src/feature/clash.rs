//! Which Clash-family cores speak which optional protocol features, and from
//! which release onwards.
//!
//! The version floors here are the whole point of the module: a core kind alone
//! cannot answer "can I use this", because every capability below arrived in a
//! specific upstream release. Each floor is pinned to the commit that
//! introduced the feature and verified against the first tag containing it.

use super::Support;
use enumset::{EnumSet, EnumSetType};
use schemars::JsonSchema;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, EnumSetType, Type, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Feature {
    /// Supports a Windows named pipe for IPC.
    NamedPipeIpc,
    /// Supports a Unix domain socket for IPC.
    UnixSocketIpc,
}

pub trait FeatureSupport {
    /// Whether `self` speaks `feature`.
    ///
    /// Pass the core's version to get a decided [`Support::Yes`] or
    /// [`Support::No`]. Pass `None` — which is also the right call for nightly
    /// builds whose version string is not semver, such as Mihomo's
    /// `alpha-<sha>` — to get the [`Support::Since`] requirement back and
    /// decide yourself. Note that a semver prerelease (`1.19.0-alpha.1`) never
    /// matches a plain floor like `>=1.18.9`, per semver's own rules.
    fn supports(&self, feature: Feature, version: Option<&Version>) -> Support;

    /// Returns the set of features `self` speaks, given a known version.
    ///
    /// If the version is unknown, returns the set of features are supported whether the version is known or not.
    fn features(&self, version: Option<&Version>) -> EnumSet<Feature> {
        let mut features = EnumSet::new();
        for feature in EnumSet::all() {
            match self.supports(feature, version) {
                Support::Yes => {
                    features.insert(feature);
                }
                Support::No => (),
                Support::Since(since) => {
                    if let Some(version) = version {
                        if since.matches(version) {
                            features.insert(feature);
                        }
                    } else {
                        features.insert(feature);
                    }
                }
            }
        }
        features
    }
}

/// Mihomo grew the two controller transports a year apart:
/// `external-controller-unix` in v1.18.4 (commit `ca84ab1a`, absent in
/// v1.18.3), `external-controller-pipe` in v1.18.9 (commit `88bfe7cf`, absent
/// in v1.18.8).
///
/// Unlike clash-rs below, neither needs a later floor for WebSocket: both
/// listeners serve the same `router()` as the TCP controller over plain
/// HTTP/1.1, so `http.Hijacker` — and with it the `/traffic`, `/memory` and
/// `/logs` upgrades — works from the release that added each key.
const MIHOMO_UNIX: &str = ">=1.18.4";
const MIHOMO_PIPE: &str = ">=1.18.9";

/// clash-rs added both keys at once in v0.9.1 (PR #867), but only the unix
/// listener was usable: it went through `axum::serve`, which always enables
/// upgrades, while the Windows named-pipe listener called
/// `hyper::server::conn::http1::Builder::serve_connection` *without*
/// `with_upgrades`. WebSocket endpoints could therefore never complete a
/// handshake over a named pipe until PR #1068 moved Windows onto `axum::serve`
/// too, first tagged in v0.9.7. That matters here because the `clash-api`
/// client drives `/traffic`, `/memory` and friends over the very same transport
/// it uses for REST, and clash-rs offers no newline-delimited-JSON fallback to
/// degrade to.
const CLASH_RS_UNIX: &str = ">=0.9.1";
const CLASH_RS_PIPE: &str = ">=0.9.7";

impl FeatureSupport for crate::kind::ClashCoreKind {
    fn supports(&self, feature: Feature, version: Option<&Version>) -> Support {
        match self {
            crate::kind::ClashCoreKind::Mihomo => match feature {
                Feature::NamedPipeIpc => since(MIHOMO_PIPE, version),
                Feature::UnixSocketIpc => since(MIHOMO_UNIX, version),
            },
            crate::kind::ClashCoreKind::ClashRust => match feature {
                Feature::NamedPipeIpc => since(CLASH_RS_PIPE, version),
                Feature::UnixSocketIpc => since(CLASH_RS_UNIX, version),
            },
            // Clash Premium only ever exposed `external-controller` over TCP.
            crate::kind::ClashCoreKind::ClashPremium => match feature {
                Feature::NamedPipeIpc | Feature::UnixSocketIpc => Support::No,
            },
            // meow-rs advertises `--ext-ctl-unix` and `--ext-ctl-pipe` in
            // `--help`, but only for mihomo CLI compatibility: both `bail!`
            // with "not yet supported" before startup (meow-app/src/main.rs,
            // v0.18.0 and current `main`). Passing either is therefore *worse*
            // than unsupported — it aborts the core instead of degrading, so
            // this must never resolve to `Yes` on the strength of the flag
            // existing. There is no YAML counterpart either: `raw.rs` has only
            // `external-controller`, parsed into a `SocketAddr`, and the repo
            // contains no `UnixListener` at all.
            crate::kind::ClashCoreKind::Meow => match feature {
                Feature::NamedPipeIpc | Feature::UnixSocketIpc => Support::No,
            },
        }
    }
}

/// Resolves a floor against a known version, or hands the requirement back when
/// the version is unknown.
fn since(req: &'static str, version: Option<&Version>) -> Support {
    let req = VersionReq::parse(req).expect("feature floor is a valid version requirement");
    match version {
        Some(version) if req.matches(version) => Support::Yes,
        Some(_) => Support::No,
        None => Support::Since(req),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::ClashCoreKind;

    fn version(raw: &str) -> Option<Version> {
        Some(Version::parse(raw).expect("test version parses"))
    }

    #[test]
    #[ignore]
    fn get_all_features() {
        for kind in [
            ClashCoreKind::Mihomo,
            ClashCoreKind::ClashRust,
            ClashCoreKind::ClashPremium,
            ClashCoreKind::Meow,
        ] {
            let features = kind.features(None);
            eprintln!("features for {kind}: {features:?}");
        }
    }

    #[test]
    fn an_unknown_version_yields_the_requirement() {
        for kind in [ClashCoreKind::Mihomo, ClashCoreKind::ClashRust] {
            for feature in [Feature::NamedPipeIpc, Feature::UnixSocketIpc] {
                assert!(matches!(kind.supports(feature, None), Support::Since(_)));
            }
        }
    }

    #[test]
    fn every_floor_brackets_the_release_that_added_its_transport() {
        for (floor, last_without, first_with) in [
            (MIHOMO_UNIX, "1.18.3", "1.18.4"),
            (MIHOMO_PIPE, "1.18.8", "1.18.9"),
            (CLASH_RS_UNIX, "0.9.0", "0.9.1"),
            (CLASH_RS_PIPE, "0.9.6", "0.9.7"),
        ] {
            assert!(
                matches!(since(floor, version(last_without).as_ref()), Support::No),
                "`{floor}` must reject {last_without}"
            );
            assert!(
                matches!(since(floor, version(first_with).as_ref()), Support::Yes),
                "`{floor}` must accept {first_with}"
            );
        }
    }

    #[test]
    fn each_core_gates_each_local_ipc_transport() {
        for (kind, feature, last_without, first_with) in [
            (
                ClashCoreKind::Mihomo,
                Feature::NamedPipeIpc,
                "1.18.8",
                "1.18.9",
            ),
            (
                ClashCoreKind::Mihomo,
                Feature::UnixSocketIpc,
                "1.18.3",
                "1.18.4",
            ),
            (
                ClashCoreKind::ClashRust,
                Feature::NamedPipeIpc,
                "0.9.6",
                "0.9.7",
            ),
            (
                ClashCoreKind::ClashRust,
                Feature::UnixSocketIpc,
                "0.9.0",
                "0.9.1",
            ),
        ] {
            assert!(
                matches!(
                    kind.supports(feature, version(last_without).as_ref()),
                    Support::No
                ),
                "{kind} must reject {feature:?} at {last_without}"
            );
            assert!(
                matches!(
                    kind.supports(feature, version(first_with).as_ref()),
                    Support::Yes
                ),
                "{kind} must accept {feature:?} at {first_with}"
            );
        }
    }

    #[test]
    fn tcp_only_cores_never_support_local_ipc() {
        for kind in [ClashCoreKind::ClashPremium, ClashCoreKind::Meow] {
            for feature in [Feature::NamedPipeIpc, Feature::UnixSocketIpc] {
                assert!(matches!(kind.supports(feature, None), Support::No));
                assert!(matches!(
                    kind.supports(feature, version("99.0.0").as_ref()),
                    Support::No
                ));
            }
        }
    }
}
