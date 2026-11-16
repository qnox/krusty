//! Dev-only selected entry points for owned C-driver fixtures. Production C ABI policy is unchanged.

/// One frontend-selected source declaration and the C fixture's explicit link name.
pub struct DriverEntry {
    pub callable: krusty::ir::CallableId,
    pub symbol: String,
}

pub(super) struct DriverExports {
    pub module: String,
    pub entries: Vec<DriverEntry>,
}
