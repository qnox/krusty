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

/// Selected declarations whose validated driver trampolines were emitted successfully.
#[derive(Default)]
pub(super) struct PublishedEntries(krusty::fx_hash::HashSet<krusty::ir::CallableId>);

impl PublishedEntries {
    pub(super) fn extend(&mut self, entries: impl IntoIterator<Item = krusty::ir::CallableId>) {
        self.0.extend(entries);
    }

    pub(super) fn contains(&self, entry: &krusty::ir::CallableId) -> bool {
        self.0.contains(entry)
    }
}
