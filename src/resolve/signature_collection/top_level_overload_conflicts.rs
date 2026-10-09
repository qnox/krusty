//! Conflict diagnostics for equally-named top-level callables.
//!
//! A top-level overload set is decided once, over the collected signature groups, and reported
//! before body checking. This module owns only that decision; it performs no lookup of its own.

use super::*;

mod registration;
pub(in crate::resolve) use registration::*;

fn report_conflicting_top_level_overloads(
    groups: &TopLevelFunctionConflictGroups,
    pending: &HashMap<TopLevelFunctionConflictDecl, PendingTopLevelFunctionConflict>,
    reserved_message_bytes: usize,
    diags: &mut DiagSink,
) -> std::collections::HashSet<TopLevelFunctionConflictKey> {
    let conflicting_keys = groups
        .groups
        .iter()
        .filter(|(_, group)| group.conflicts)
        .map(|(key, _)| key.clone())
        .collect();
    let mut retained_message_bytes = reserved_message_bytes;
    let saved_file = diags.current_file();
    let mut pending = pending.iter().collect::<Vec<_>>();
    pending.sort_unstable_by_key(|(declaration, _)| {
        (
            declaration.file,
            declaration.diagnostic_span.lo,
            declaration.diagnostic_span.hi,
        )
    });
    for (&current, &pending_conflict) in pending {
        diags.set_file(current.file);
        let Some((_, group)) = groups.groups.get(pending_conflict.group) else {
            continue;
        };
        let file_candidates = group
            .files
            .get(&current.file)
            .map(|state| state.candidates.as_slice())
            .unwrap_or_default();
        let public_candidates = group
            .public_candidates
            .iter()
            .filter(|candidate| {
                pending_conflict.include_cross_file_public || candidate.file == current.file
            })
            .collect::<Vec<_>>();
        let first_public = public_candidates.iter().copied().find(|candidate| {
            candidate.file != current.file || candidate.declaration != current.declaration
        });
        let first_file = file_candidates.iter().find(|candidate| {
            candidate.file != current.file || candidate.declaration != current.declaration
        });
        let mut displays = Vec::with_capacity(MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES);
        let mut seen = std::collections::HashSet::new();
        let mut display_bytes = 0usize;
        for candidate in first_public
            .into_iter()
            .chain(first_file)
            .chain(public_candidates.iter().copied())
            .chain(file_candidates)
        {
            if displays.len() >= MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES {
                break;
            }
            if candidate.file == current.file && candidate.declaration == current.declaration {
                continue;
            }
            if !seen.insert((candidate.file, candidate.declaration)) {
                continue;
            }
            if CONFLICTING_OVERLOAD_PREFIX
                .len()
                .saturating_add(display_bytes)
                .saturating_add(1)
                .saturating_add(candidate.display.len())
                > MAX_OVERLOAD_DIAGNOSTIC_BYTES
                || retained_message_bytes
                    .saturating_add(display_bytes)
                    .saturating_add(1)
                    .saturating_add(candidate.display.len())
                    > MAX_CONFLICTING_OVERLOAD_DIAGNOSTIC_BYTES
            {
                break;
            }
            display_bytes += 1 + candidate.display.len();
            displays.push(candidate.display.as_str());
        }
        let message = if displays.is_empty() {
            CONFLICTING_OVERLOAD_PREFIX.to_string()
        } else {
            displays.sort_unstable();
            let mut message =
                String::with_capacity(CONFLICTING_OVERLOAD_PREFIX.len() + display_bytes);
            message.push_str(CONFLICTING_OVERLOAD_PREFIX);
            for display in displays {
                message.push('\n');
                message.push_str(display);
            }
            message
        };
        retained_message_bytes += display_bytes;
        diags.error(current.diagnostic_span, message);
    }
    diags.set_file(saved_file);
    conflicting_keys
}

pub(in crate::resolve) fn commit_top_level_conflict_groups(
    table: &mut SymbolTable,
    groups: &TopLevelFunctionConflictGroups,
    pending: &HashMap<TopLevelFunctionConflictDecl, PendingTopLevelFunctionConflict>,
    reserved_message_bytes: usize,
    diags: &mut DiagSink,
) {
    table.conflicting_top_level_keys =
        report_conflicting_top_level_overloads(groups, pending, reserved_message_bytes, diags);
    table.conflicting_top_level_candidates = groups
        .groups
        .iter()
        .filter(|(key, _)| table.conflicting_top_level_keys.contains(key))
        .map(|(key, group)| {
            (
                key.clone(),
                TopLevelFunctionConflictCandidates {
                    public: group.public_candidates.clone(),
                    by_file: group
                        .files
                        .iter()
                        .filter(|&(_, state)| !state.candidates.is_empty())
                        .map(|(&file, state)| (file, state.candidates.clone()))
                        .collect(),
                },
            )
        })
        .collect();
}

/// Publish the exact source-to-conflict-key relation used by invalid-call recovery.
///
/// This runs only for the legacy AST entry point. A signature without its declaration is omitted:
/// recovery must not guess a declared generic arity from the normalized signature payload.
pub(in crate::resolve) fn publish_legacy_top_level_conflict_recovery_keys(
    table: &mut SymbolTable,
    files: &[File],
) {
    table.conflicting_top_level_key_by_source.clear();
    for (name, signatures) in &table.funs {
        for signature in signatures {
            let Some((file, declaration)) = signature.source_file.zip(signature.source_decl) else {
                continue;
            };
            let Some(function) =
                files
                    .get(file as usize)
                    .and_then(|source| match source.decl(declaration) {
                        Decl::Fun(function) => Some(function),
                        _ => None,
                    })
            else {
                continue;
            };
            let Some(key) = TopLevelFunctionConflictKey::from_signature(
                signature,
                name.clone(),
                function.type_params.len(),
            ) else {
                continue;
            };
            let local = signature.visibility.is_private()
                || is_kotlin_main_entry_point(function, &signature.params, signature.ret);
            let retained = table
                .conflicting_top_level_candidates
                .get(&key)
                .is_some_and(|candidates| !local || candidates.by_file.contains_key(&file));
            if retained {
                table
                    .conflicting_top_level_key_by_source
                    .insert((file, declaration.0), key);
            }
        }
    }
    for (name, receivers) in &table.ext_funs {
        for signatures in receivers.values() {
            for signature in signatures {
                let Some((file, declaration, _receiver)) = signature
                    .source_file
                    .zip(signature.source_decl)
                    .zip(signature.source_receiver)
                    .map(|((file, declaration), receiver)| (file, declaration, receiver))
                else {
                    continue;
                };
                let Some(declared_type_parameter_count) =
                    files
                        .get(file as usize)
                        .and_then(|source| match source.decl(declaration) {
                            Decl::Fun(function) => Some(function.type_params.len()),
                            _ => None,
                        })
                else {
                    continue;
                };
                let Some(key) = TopLevelFunctionConflictKey::from_signature(
                    signature,
                    name.clone(),
                    declared_type_parameter_count,
                ) else {
                    continue;
                };
                let retained = table
                    .conflicting_top_level_candidates
                    .get(&key)
                    .is_some_and(|candidates| {
                        !signature.visibility.is_private() || candidates.by_file.contains_key(&file)
                    });
                if retained {
                    table
                        .conflicting_top_level_key_by_source
                        .insert((file, declaration.0), key);
                }
            }
        }
    }
}
