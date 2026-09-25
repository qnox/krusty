//! JVM-local compaction for parameters removed by an inline splice.
//!
//! A removed `long` or `double` closes two physical slots. Keeping that width in one layout object
//! prevents instruction locals, debug locals, and the caller's `top_local` from disagreeing.

use super::param_store_ops;

#[derive(Clone, Debug)]
pub(super) struct LocalCompaction {
    removed: Vec<RemovedParameter>,
}

#[derive(Clone, Copy, Debug)]
struct RemovedParameter {
    start: u16,
    width: u16,
}

impl LocalCompaction {
    pub(super) fn parameters(descriptor: &str, indices: &[usize]) -> Option<Self> {
        let parameters = parameter_slots(descriptor)?;
        let removed = indices
            .iter()
            .map(|&index| parameters.get(index).copied())
            .collect::<Option<Vec<_>>>()?;
        Some(Self { removed })
    }

    pub(super) fn is_removed(&self, slot: u16) -> bool {
        self.removed
            .iter()
            .any(|parameter| (parameter.start..parameter.start + parameter.width).contains(&slot))
    }

    /// Relocate a retained local. Callers may also use this while rewriting instructions that are
    /// scheduled for deletion; a removed parameter then maps to the start of its closed gap.
    pub(super) fn compact(&self, slot: u16, base: u16) -> u16 {
        let closed = self
            .removed
            .iter()
            .filter(|parameter| parameter.start < slot)
            .map(|parameter| parameter.width.min(slot - parameter.start))
            .sum::<u16>();
        base + slot - closed
    }
}

fn parameter_slots(descriptor: &str) -> Option<Vec<RemovedParameter>> {
    param_store_ops(descriptor, 0).map(|stores| {
        stores
            .into_iter()
            .map(|(start, op)| RemovedParameter {
                start,
                width: if matches!(op, 0x37 | 0x39) { 2 } else { 1 },
            })
            .collect()
    })
}
