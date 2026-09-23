//! Relocation of dependency and caller line marks into one spliced JVM body.

use super::{assemble, LambdaBody};

/// The instruction-layout facts needed to relocate line marks after a host body and its literal
/// lambda arguments have been rewritten into one instruction sequence.
pub(super) struct Relocation<'a> {
    pub(super) host_lines: &'a [(u16, u16)],
    pub(super) original_offsets: &'a [usize],
    pub(super) old_to_new: &'a [usize],
    pub(super) prologue_len: usize,
    pub(super) output_offsets: &'a [usize],
    pub(super) output_instruction_count: usize,
    pub(super) lambda_sites: &'a [usize],
    pub(super) site_bodies: &'a [&'a LambdaBody],
    pub(super) dropped_prefix: &'a [usize],
    pub(super) dropped_suffix: &'a [usize],
}

/// Produce `(absolute offset, line, from dependency)` marks in output-offset order.
///
/// A host mark at the exact end of the splice belonged to its removed trailing return and marks no
/// surviving instruction. Lambda marks name caller source. When a lambda returns, the dependency
/// line in effect before it resumes at the end of the substituted body.
pub(super) fn relocate(input: Relocation<'_>) -> Option<Vec<(u16, u16, bool)>> {
    let Relocation {
        host_lines,
        original_offsets,
        old_to_new,
        prologue_len,
        output_offsets,
        output_instruction_count,
        lambda_sites,
        site_bodies,
        dropped_prefix,
        dropped_suffix,
    } = input;
    let byte_to_index = |offset: u16| {
        original_offsets
            .iter()
            .position(|&at| at == usize::from(offset))
    };
    let spliced_end = *output_offsets.get(output_instruction_count)?;
    let mut relocated = Vec::new();
    for &(start_pc, line) in host_lines {
        let Some(index) = byte_to_index(start_pc) else {
            continue;
        };
        let rewritten = prologue_len.checked_add(*old_to_new.get(index)?)?;
        let at = *output_offsets.get(rewritten)?;
        if at >= spliced_end {
            continue;
        }
        let Ok(at) = u16::try_from(at) else {
            continue;
        };
        relocated.push((at, line, true));
    }
    for (occurrence, &site) in lambda_sites.iter().enumerate() {
        let rewritten = prologue_len.checked_add(*old_to_new.get(site)?)?;
        let body_start = *output_offsets.get(rewritten)?;
        let prefix = *dropped_prefix.get(occurrence)?;
        let lambda = *site_bodies.get(occurrence)?;
        for &(at, line) in &lambda.lines {
            let Some(offset) = usize::from(at).checked_sub(prefix) else {
                continue;
            };
            let Some(at) = body_start.checked_add(offset) else {
                continue;
            };
            let Ok(at) = u16::try_from(at) else {
                continue;
            };
            relocated.push((at, line, false));
        }
    }

    let mut resumed = Vec::new();
    for (occurrence, &site) in lambda_sites.iter().enumerate() {
        let lambda = *site_bodies.get(occurrence)?;
        if lambda.lines.is_empty() {
            continue;
        }
        let rewritten = prologue_len.checked_add(*old_to_new.get(site)?)?;
        let body_start = *output_offsets.get(rewritten)?;
        let Some(removed) = dropped_prefix
            .get(occurrence)?
            .checked_add(*dropped_suffix.get(occurrence)?)
        else {
            continue;
        };
        let Some(spliced_len) = assemble(&lambda.body).len().checked_sub(removed) else {
            continue;
        };
        let Some(end) = body_start.checked_add(spliced_len) else {
            continue;
        };
        let Ok(end) = u16::try_from(end) else {
            continue;
        };
        let host_line = relocated
            .iter()
            .filter(|&&(at, _, from_dependency)| from_dependency && usize::from(at) <= body_start)
            .max_by_key(|&&(at, _, _)| at)
            .map(|&(_, line, _)| line);
        if let Some(line) = host_line {
            resumed.push((end, line, true));
        }
    }
    relocated.extend(resumed);
    relocated.sort_by_key(|&(at, _, _)| at);
    Some(relocated)
}
