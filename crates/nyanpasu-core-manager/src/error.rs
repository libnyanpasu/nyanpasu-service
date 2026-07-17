use crate::kind::CoreKind;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("core kind `{0}` has no launch profile yet")]
    UnsupportedCore(CoreKind),
}
