#[derive(Debug, Clone)]
pub enum ClashCoreType {
    Mihomo,
    MihomoAlpha,
    ClashRust,
    ClashPremium,
}

#[derive(Debug, Clone)]
pub enum CoreType {
    Clash(ClashCoreType),
    SingBox, // Maybe we would support this in the 2.x?
}
