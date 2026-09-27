//! Stable post-solver classification of top-level callable conflicts.

use super::super::*;

/// Classify top-level overload conflicts after the compact signature graph has finalized every
/// inferred result. This is part of Pass 1: it consumes stable declaration headers and semantic
/// signatures only, emits diagnostics, and leaves no diagnostic origin or temporary graph state
/// for Pass 2.
pub(crate) fn finalize_streamed_top_level_conflicts(
    headers: &crate::fir::StreamedHeaderModule,
    table: &mut SymbolTable,
    diags: &mut DiagSink,
) {
    #[derive(Clone)]
    struct Entry {
        declaration: crate::fir::DeclarationId,
        source: u32,
        name: String,
        signature: Signature,
        entry_point: bool,
    }

    let mut entries = Vec::new();
    for stub in headers.stubs.iter().filter(|stub| {
        stub.kind == crate::fir::DeclarationKind::Function
            && headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner.is_none())
    }) {
        let Some(name) = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
        else {
            continue;
        };
        let signature = table
            .funs
            .get(name)
            .into_iter()
            .flatten()
            .chain(
                table
                    .ext_funs
                    .get(name)
                    .into_iter()
                    .flat_map(HashMap::values)
                    .flatten(),
            )
            .find(|signature| signature.stable_declaration == Some(stub.id))
            .cloned();
        let Some(signature) = signature else {
            continue;
        };
        let header = streamed_callable_header_by_declaration(headers, stub.id)
            .expect("a top-level function must retain its compact callable header");
        let entry_point = crate::fir::MainEntryShape {
            name,
            has_extension_receiver: header.receiver.is_some(),
            type_parameter_count: header.type_parameters.len(),
            context_parameter_count: header.context_count,
            parameters: &signature.params,
            result: signature.ret,
        }
        .entry_parameters()
        .is_some();
        entries.push(Entry {
            declaration: stub.id,
            source: stub.source.raw(),
            name: name.to_string(),
            signature,
            entry_point,
        });
    }

    let mut groups = TopLevelFunctionConflictGroups::default();
    let mut pending = HashMap::new();
    let mut reserved_diagnostic_bytes = 0usize;
    let mut retained_display_bytes = 0usize;
    for entry in &entries {
        let Some(key) =
            TopLevelFunctionConflictKey::from_signature(&entry.signature, entry.name.clone())
        else {
            continue;
        };
        register_top_level_function_conflict(
            TopLevelFunctionConflictDisplaySource::Compact(headers),
            &mut groups,
            TopLevelFunctionConflictRegistration {
                key,
                declaration: TopLevelFunctionConflictDecl {
                    file: entry.source,
                    declaration: TopLevelFunctionConflictDeclaration::Stable(entry.declaration),
                    diagnostic_span: streamed_callable_signature_span(headers, entry.declaration)
                        .expect("a top-level callable must retain its signature origin"),
                },
                private: entry.signature.visibility.is_private(),
                entry_point: entry.entry_point,
            },
            &mut pending,
            &mut reserved_diagnostic_bytes,
            &mut retained_display_bytes,
        );
    }

    commit_top_level_conflict_groups(table, &groups, &pending, reserved_diagnostic_bytes, diags);
    table.conflicting_top_level_key_by_source.clear();
    for entry in entries {
        let Some(source_declaration) = entry.signature.source_decl else {
            continue;
        };
        let Some(key) = TopLevelFunctionConflictKey::from_signature(&entry.signature, entry.name)
        else {
            continue;
        };
        let local = entry.signature.visibility.is_private() || entry.entry_point;
        let retained_for_recovery = table
            .conflicting_top_level_candidates
            .get(&key)
            .is_some_and(|candidates| !local || candidates.by_file.contains_key(&entry.source));
        if retained_for_recovery {
            table
                .conflicting_top_level_key_by_source
                .insert((entry.source, source_declaration.0), key);
        }
    }
}
