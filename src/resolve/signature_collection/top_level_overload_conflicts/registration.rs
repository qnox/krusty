//! Registration and bounded diagnostic state for conflicting top-level functions.
//!
//! Both legacy AST signature collection and compact streamed-header finalization feed this
//! common classifier. Reporting remains in the parent module.

use super::*;
use crate::resolve::source_signature_display::{
    source_function_conflict_display, streamed_function_conflict_display,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::resolve) enum TopLevelFunctionConflictDeclaration {
    Legacy(DeclId),
    Stable(crate::fir::DeclarationId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::resolve) struct TopLevelFunctionConflictDecl {
    pub(in crate::resolve) file: u32,
    pub(in crate::resolve) declaration: TopLevelFunctionConflictDeclaration,
    pub(in crate::resolve) diagnostic_span: Span,
}

#[derive(Clone)]
pub(in crate::resolve) struct TopLevelFunctionConflictCandidate {
    pub(in crate::resolve) file: u32,
    pub(in crate::resolve) declaration: TopLevelFunctionConflictDeclaration,
    pub(in crate::resolve) display: String,
}

#[derive(Clone, Copy)]
pub(in crate::resolve) enum TopLevelFunctionConflictDisplaySource<'a> {
    Legacy(&'a [File]),
    Compact(&'a crate::fir::StreamedHeaderModule),
}

#[derive(Default)]
pub(super) struct TopLevelFunctionConflictGroup {
    /// Cross-file-visible, non-entry-point declarations. Entry points and file-private declarations
    /// still participate in their declaring file's candidate list below.
    pub(super) public_candidates: Vec<TopLevelFunctionConflictCandidate>,
    pub(super) public_count: usize,
    pub(super) retained_public_count: usize,
    pub(super) first_public: Option<TopLevelFunctionConflictDecl>,
    pub(super) conflicts: bool,
    pub(super) files: HashMap<u32, TopLevelFunctionConflictFile>,
}

#[derive(Default)]
pub(super) struct TopLevelFunctionConflictFile {
    pub(super) declaration_count: usize,
    pub(super) retained_local_count: usize,
    pub(super) first_declaration: Option<TopLevelFunctionConflictDecl>,
    pub(super) first_public: Option<TopLevelFunctionConflictDecl>,
    pub(super) first_local: Option<TopLevelFunctionConflictDecl>,
    pub(super) ordinary_private_declarations: Vec<TopLevelFunctionConflictDecl>,
    pub(super) candidates: Vec<TopLevelFunctionConflictCandidate>,
}

#[derive(Default)]
pub(in crate::resolve) struct TopLevelFunctionConflictGroups {
    indexes: HashMap<TopLevelFunctionConflictKey, usize>,
    pub(super) groups: Vec<(TopLevelFunctionConflictKey, TopLevelFunctionConflictGroup)>,
}

impl TopLevelFunctionConflictGroups {
    fn get_or_insert(&mut self, key: TopLevelFunctionConflictKey) -> usize {
        if let Some(&index) = self.indexes.get(&key) {
            return index;
        }
        let index = self.groups.len();
        self.indexes.insert(key.clone(), index);
        self.groups
            .push((key, TopLevelFunctionConflictGroup::default()));
        index
    }
}

#[derive(Clone, Default)]
pub(in crate::resolve) struct TopLevelFunctionConflictCandidates {
    pub(in crate::resolve) public: Vec<TopLevelFunctionConflictCandidate>,
    pub(in crate::resolve) by_file: HashMap<u32, Vec<TopLevelFunctionConflictCandidate>>,
}

#[derive(Clone, Copy)]
pub(in crate::resolve) struct PendingTopLevelFunctionConflict {
    pub(super) group: usize,
    pub(super) include_cross_file_public: bool,
}

pub(in crate::resolve) struct TopLevelFunctionConflictRegistration {
    pub(in crate::resolve) key: TopLevelFunctionConflictKey,
    pub(in crate::resolve) declaration: TopLevelFunctionConflictDecl,
    pub(in crate::resolve) private: bool,
    pub(in crate::resolve) entry_point: bool,
}

fn retain_conflict_diagnostic(
    pending: &mut HashMap<TopLevelFunctionConflictDecl, PendingTopLevelFunctionConflict>,
    reserved_bytes: &mut usize,
    declaration: TopLevelFunctionConflictDecl,
    group: usize,
    include_cross_file_public: bool,
) {
    if let Some(existing) = pending.get_mut(&declaration) {
        existing.include_cross_file_public |= include_cross_file_public;
        return;
    }
    if reserved_bytes.saturating_add(CONFLICTING_OVERLOAD_PREFIX.len())
        > MAX_CONFLICTING_OVERLOAD_DIAGNOSTIC_BYTES
    {
        return;
    }
    *reserved_bytes += CONFLICTING_OVERLOAD_PREFIX.len();
    pending.insert(
        declaration,
        PendingTopLevelFunctionConflict {
            group,
            include_cross_file_public,
        },
    );
}

fn retain_conflict_candidate(
    source: TopLevelFunctionConflictDisplaySource<'_>,
    candidates: &mut Vec<TopLevelFunctionConflictCandidate>,
    retained_display_bytes: &mut usize,
    declaration: TopLevelFunctionConflictDecl,
) {
    if candidates.len() > MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES
        || candidates.iter().any(|candidate| {
            candidate.file == declaration.file && candidate.declaration == declaration.declaration
        })
        || *retained_display_bytes >= MAX_CONFLICTING_OVERLOAD_DIAGNOSTIC_BYTES
    {
        return;
    }
    let remaining_bytes =
        MAX_CONFLICTING_OVERLOAD_DIAGNOSTIC_BYTES.saturating_sub(*retained_display_bytes);
    let per_message_bytes =
        MAX_OVERLOAD_DIAGNOSTIC_BYTES.saturating_sub(INAPPLICABLE_OVERLOAD_PREFIX.len() + 2);
    let max_bytes = remaining_bytes.min(per_message_bytes);
    let display =
        match (source, declaration.declaration) {
            (
                TopLevelFunctionConflictDisplaySource::Legacy(files),
                TopLevelFunctionConflictDeclaration::Legacy(declaration_id),
            ) => files.get(declaration.file as usize).and_then(|file| {
                match file.decl(declaration_id) {
                    Decl::Fun(function) => {
                        source_function_conflict_display(file, function, max_bytes)
                    }
                    Decl::Property(_) | Decl::Class(_) => None,
                }
            }),
            (
                TopLevelFunctionConflictDisplaySource::Compact(headers),
                TopLevelFunctionConflictDeclaration::Stable(declaration_id),
            ) => streamed_function_conflict_display(headers, declaration_id, max_bytes),
            _ => None,
        };
    let Some(display) = display else {
        *retained_display_bytes = MAX_CONFLICTING_OVERLOAD_DIAGNOSTIC_BYTES;
        return;
    };
    *retained_display_bytes += display.len();
    candidates.push(TopLevelFunctionConflictCandidate {
        file: declaration.file,
        declaration: declaration.declaration,
        display,
    });
}

pub(in crate::resolve) fn is_kotlin_main_entry_point(
    function: &FunDecl,
    params: &[Ty],
    ret: Ty,
) -> bool {
    crate::fir::MainEntryShape {
        name: &function.name,
        has_extension_receiver: function.receiver.is_some(),
        type_parameter_count: function.type_params.len(),
        context_parameter_count: function.context_count,
        parameters: params,
        result: ret,
    }
    .entry_parameters()
    .is_some()
}

pub(in crate::resolve) fn register_top_level_function_conflict(
    source: TopLevelFunctionConflictDisplaySource<'_>,
    groups: &mut TopLevelFunctionConflictGroups,
    registration: TopLevelFunctionConflictRegistration,
    pending: &mut HashMap<TopLevelFunctionConflictDecl, PendingTopLevelFunctionConflict>,
    reserved_diagnostic_bytes: &mut usize,
    retained_display_bytes: &mut usize,
) -> bool {
    let TopLevelFunctionConflictRegistration {
        key,
        declaration: current,
        private,
        entry_point,
    } = registration;
    let group_index = groups.get_or_insert(key);
    let group = &mut groups.groups[group_index].1;
    let file_state = group.files.get(&current.file);
    let first_in_file = file_state.and_then(|state| state.first_declaration);
    let first_file_public = file_state.and_then(|state| state.first_public);
    let first_file_local = file_state.and_then(|state| state.first_local);
    let retained_local_count = file_state
        .map(|state| state.retained_local_count)
        .unwrap_or_default();
    let local = private || entry_point;
    let retained = if local {
        retained_local_count < MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES
    } else {
        group.retained_public_count < MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES
    };

    // Every declaration shares its file's top-level scope, including an entry point or a private
    // declaration. Cross-file exceptions never suppress a same-file conflict.
    if let Some(first) = first_in_file {
        group.conflicts = true;
        retain_conflict_diagnostic(
            pending,
            reserved_diagnostic_bytes,
            first,
            group_index,
            false,
        );
        retain_conflict_diagnostic(
            pending,
            reserved_diagnostic_bytes,
            current,
            group_index,
            false,
        );
        if let Some(first_public) = first_file_public {
            retain_conflict_candidate(
                source,
                &mut group.public_candidates,
                retained_display_bytes,
                first_public,
            );
        }
        if let Some(first_local) = first_file_local {
            retain_conflict_candidate(
                source,
                &mut group.files.entry(current.file).or_default().candidates,
                retained_display_bytes,
                first_local,
            );
        }
        if local {
            retain_conflict_candidate(
                source,
                &mut group.files.entry(current.file).or_default().candidates,
                retained_display_bytes,
                current,
            );
        } else {
            retain_conflict_candidate(
                source,
                &mut group.public_candidates,
                retained_display_bytes,
                current,
            );
        }
    }

    // A real application entry point is omitted from cross-file overload-conflict consideration.
    // An ordinary private declaration is isolated only from declarations in other private files;
    // a package-visible declaration is also visible in the private declaration's file and conflicts
    // there. The diagnostic therefore belongs to the private declaration, not the public one.
    if !entry_point && private && group.public_count > 0 {
        group.conflicts = true;
        retain_conflict_diagnostic(
            pending,
            reserved_diagnostic_bytes,
            current,
            group_index,
            true,
        );
        if let Some(first_public) = group.first_public {
            retain_conflict_candidate(
                source,
                &mut group.public_candidates,
                retained_display_bytes,
                first_public,
            );
        }
        retain_conflict_candidate(
            source,
            &mut group.files.entry(current.file).or_default().candidates,
            retained_display_bytes,
            current,
        );
    }

    if !entry_point && !private {
        if group.public_count > 0 {
            group.conflicts = true;
            if let Some(first_public) = group.first_public {
                retain_conflict_diagnostic(
                    pending,
                    reserved_diagnostic_bytes,
                    first_public,
                    group_index,
                    true,
                );
                retain_conflict_candidate(
                    source,
                    &mut group.public_candidates,
                    retained_display_bytes,
                    first_public,
                );
            }
            retain_conflict_diagnostic(
                pending,
                reserved_diagnostic_bytes,
                current,
                group_index,
                true,
            );
            retain_conflict_candidate(
                source,
                &mut group.public_candidates,
                retained_display_bytes,
                current,
            );
        } else {
            // This is the first package-visible ordinary declaration. Any ordinary private
            // declarations collected earlier now conflict in their own files. Retain only the
            // bounded declaration inventory needed by the bounded diagnostic surface.
            let prior_private = group
                .files
                .iter()
                .filter(|(&file, _)| file != current.file)
                .flat_map(|(_, state)| state.ordinary_private_declarations.iter().copied())
                .collect::<Vec<_>>();
            if !prior_private.is_empty() {
                group.conflicts = true;
                retain_conflict_candidate(
                    source,
                    &mut group.public_candidates,
                    retained_display_bytes,
                    current,
                );
                for declaration in prior_private {
                    retain_conflict_diagnostic(
                        pending,
                        reserved_diagnostic_bytes,
                        declaration,
                        group_index,
                        true,
                    );
                    retain_conflict_candidate(
                        source,
                        &mut group.files.entry(declaration.file).or_default().candidates,
                        retained_display_bytes,
                        declaration,
                    );
                }
            }
        }
    }

    let file_state = group.files.entry(current.file).or_default();
    file_state.declaration_count += 1;
    file_state.first_declaration.get_or_insert(current);
    if local {
        file_state.retained_local_count += usize::from(retained);
        file_state.first_local.get_or_insert(current);
        if !entry_point
            && file_state.ordinary_private_declarations.len() < MAX_OVERLOAD_DIAGNOSTIC_CANDIDATES
        {
            file_state.ordinary_private_declarations.push(current);
        }
    } else {
        file_state.first_public.get_or_insert(current);
        group.retained_public_count += usize::from(retained);
        group.public_count += 1;
        group.first_public.get_or_insert(current);
    }
    retained
}
