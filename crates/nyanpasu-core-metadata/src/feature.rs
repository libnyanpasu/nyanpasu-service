use semver::VersionReq;

pub mod clash;

#[derive(Debug)]
pub enum Support {
    Yes,
    /// Since `VersionReq`, the core supports this feature.
    Since(VersionReq),
    No,
}
