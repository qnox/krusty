//! Candidate retention policy for bounded overload diagnostics.

use crate::libraries::FunctionInfo;

/// Keep visible public and same-file declarations represented before filling the diagnostic cap.
///
/// The finalized provider has no parser-backed conflict-display cache. A source-ordered prefix can
/// otherwise spend the complete budget on one visibility family and omit another family which was
/// considered at the call site.
pub(super) fn visibility_diverse_candidate_indices(
    candidates: &[FunctionInfo],
    source_file: u32,
    limit: usize,
) -> Vec<usize> {
    let mut order = Vec::with_capacity(limit);
    let mut retain = |index: usize| {
        if order.len() < limit && !order.contains(&index) {
            order.push(index);
        }
    };
    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate.visibility.is_public())
    {
        retain(index);
    }
    if let Some(index) = candidates.iter().position(|candidate| {
        !candidate.visibility.is_public() && candidate.source_file == Some(source_file)
    }) {
        retain(index);
    }
    for (index, _) in candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.visibility.is_public())
    {
        retain(index);
    }
    for (index, _) in candidates.iter().enumerate().filter(|(_, candidate)| {
        !candidate.visibility.is_public() && candidate.source_file == Some(source_file)
    }) {
        retain(index);
    }
    for index in 0..candidates.len() {
        retain(index);
    }
    order
}
