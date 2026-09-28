//! Field order of a local or anonymous class's captures across repeated checks of its body.
//!
//! Postponed generic-lambda checking may revisit the same construction while one receiver type is
//! temporarily `Pending`. A provisional revisit must neither replace the exact symbolic type
//! recorded by the earlier check nor renumber an established capture field: descendant
//! constructions can already carry one of these ordinals as their resolved `ClassStorage` source.
//! The final check takes the order it selected (kotlinc's first-use order) and returns the field
//! permutation, so direct descendants update their exact storage coordinates instead of the class
//! body being checked again. This table crosses the retained-inline boundary and no pending
//! semantic type may reach checked FIR.

use super::{AnonymousObjectCapture, AnonymousObjectCaptureSource};

/// Reconcile `selected` with the fields a previous check `established`. Returns the capture list
/// and, for each established field, its new ordinal (`None` when the capture was dropped).
pub(super) fn reconcile(
    established: Option<&[AnonymousObjectCapture]>,
    mut selected: Vec<AnonymousObjectCapture>,
    provisional: bool,
) -> (Vec<AnonymousObjectCapture>, Vec<Option<u32>>) {
    let Some(established) = established else {
        return (selected, Vec::new());
    };
    let mut field_remap = vec![None; established.len()];
    if !provisional {
        for (field, exact) in established.iter().enumerate() {
            if let Some(position) = selected
                .iter()
                .position(|capture| same_capture(capture, exact))
            {
                keep_established_facts(&mut selected[position], exact);
                field_remap[field] = u32::try_from(position).ok();
            }
        }
        return (selected, field_remap);
    }
    let mut pending = selected;
    let mut selected = Vec::with_capacity(established.len().max(pending.len()));
    for (field, exact) in established.iter().enumerate() {
        let Some(position) = pending
            .iter()
            .position(|capture| same_capture(capture, exact))
        else {
            field_remap[field] = u32::try_from(selected.len()).ok();
            selected.push(exact.clone());
            continue;
        };
        let mut capture = pending.remove(position);
        keep_established_facts(&mut capture, exact);
        field_remap[field] = u32::try_from(selected.len()).ok();
        selected.push(capture);
    }
    selected.extend(pending);
    (selected, field_remap)
}

fn same_capture(capture: &AnonymousObjectCapture, exact: &AnonymousObjectCapture) -> bool {
    capture.name == exact.name && capture.source == exact.source
}

/// A capture keeps its established cell sharing, and its established type while the revisit only
/// knows a pending one.
fn keep_established_facts(capture: &mut AnonymousObjectCapture, exact: &AnonymousObjectCapture) {
    capture.shared_cell |= exact.shared_cell;
    if (capture.ty.mentions_pending()
        || capture
            .storage_ty
            .is_some_and(|storage| storage.mentions_pending()))
        && !exact.ty.mentions_pending()
        && exact
            .storage_ty
            .is_none_or(|storage| !storage.mentions_pending())
    {
        capture.ty = exact.ty;
        capture.storage_ty = exact.storage_ty;
    }
}

/// Put a local class's used receiver captures in the order its body first used them, as kotlinc
/// does, keeping every other capture in its field. `used` pairs each receiver's first-use position
/// with its capture source.
pub(super) fn order_receivers_by_first_use(
    captures: &mut [AnonymousObjectCapture],
    bindings: &mut [Option<u32>],
    mut used: Vec<(usize, AnonymousObjectCaptureSource)>,
) {
    used.sort_by_key(|(first_use, _)| *first_use);
    let fields = captures
        .iter()
        .enumerate()
        .filter(|(_, capture)| used.iter().any(|(_, source)| *source == capture.source))
        .map(|(field, _)| field)
        .collect::<Vec<_>>();
    let mut ordered = fields
        .iter()
        .map(|&field| (captures[field].clone(), bindings[field]))
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(capture, _)| {
        used.iter()
            .position(|(_, source)| *source == capture.source)
    });
    for (field, (capture, binding)) in fields.into_iter().zip(ordered) {
        captures[field] = capture;
        bindings[field] = binding;
    }
}
