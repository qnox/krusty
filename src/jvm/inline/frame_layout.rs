//! Caller-frame layout for an inline splice.

use super::in_place_arguments::InPlacePlan;
use super::local_compaction::LocalCompaction;
use super::{
    decode_stackmap, disassemble, free_local_slot_at, lambda_invoke_sites, null_check_deletions,
    old_offsets, param_offsets, param_store_ops, param_vtypes_full, Frame, MethodCode,
};

/// How a splice will lay out the caller's frame: where each substituted lambda's own locals begin,
/// and how far the relocated host body reaches.
///
/// Both answers come from one compaction rule shared with instruction, frame, and debug-local
/// rewriting, so a removed wide parameter closes the same two slots everywhere.
pub(in crate::jvm) struct SplicedFrame {
    /// One entry per `lambda_params` position, in that order.
    pub(in crate::jvm) lambda_bases: Vec<u16>,
    /// How many `FunctionN.invoke` sites the host has for each `lambda_params` position.
    pub(in crate::jvm) site_counts: Vec<usize>,
    /// One past the highest caller slot the relocated host body occupies.
    pub(in crate::jvm) top_local: u16,
}

/// Plan the caller frame for splicing `body`. `None` when the body cannot be decoded or a lambda
/// parameter has no `FunctionN.invoke` this splice would replace.
pub(in crate::jvm) fn spliced_frame(
    body: &MethodCode,
    descriptor: &str,
    lambda_params: &[usize],
    in_place: Option<&InPlacePlan>,
    base: u16,
) -> Option<SplicedFrame> {
    let offsets_of_param = param_offsets(descriptor)?;
    let insns = disassemble(&body.code)?;
    let deleted = null_check_deletions(&insns, &body.source_cp);
    let lambda_slots = lambda_params
        .iter()
        .enumerate()
        .map(|(lambda, &parameter)| {
            offsets_of_param
                .get(parameter)
                .copied()
                .map(|slot| (lambda, slot))
        })
        .collect::<Option<Vec<_>>>()?;
    let compaction = if let Some(plan) = in_place {
        plan.compaction().clone()
    } else {
        LocalCompaction::parameters(descriptor, lambda_params)?
    };
    let parameter_end = param_store_ops(descriptor, 0)?
        .last()
        .map(|&(slot, op)| slot + if matches!(op, 0x37 | 0x39) { 2 } else { 1 })
        .unwrap_or_default();
    let frames: Vec<(usize, Frame)> = match body.stackmap.as_ref() {
        Some(stackmap) => {
            let frame0 = param_vtypes_full(descriptor, &body.source_cp)?;
            let offsets = old_offsets(&body.code)?;
            decode_stackmap(stackmap, frame0)?
                .into_iter()
                .map(|frame| {
                    offsets
                        .iter()
                        .position(|&at| at == frame.offset)
                        .map(|index| (index, frame))
                })
                .collect::<Option<Vec<_>>>()?
        }
        None => Vec::new(),
    };
    let sites = lambda_invoke_sites(&insns, &body.source_cp, &lambda_slots, &deleted)?;
    let mut bases = vec![None; lambda_params.len()];
    let mut site_counts = vec![0usize; lambda_params.len()];
    for (lambda, _, site) in sites {
        let free = free_local_slot_at(&insns, site, &frames, parameter_end, body.max_locals);
        let at = compaction.compact(free, base);
        bases[lambda] = Some(bases[lambda].map_or(at, |had: u16| had.max(at)));
        site_counts[lambda] += 1;
    }
    Some(SplicedFrame {
        lambda_bases: bases.into_iter().collect::<Option<Vec<_>>>()?,
        site_counts,
        top_local: compaction.compact(body.max_locals, base),
    })
}
