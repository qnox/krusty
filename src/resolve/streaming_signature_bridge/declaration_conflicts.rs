//! Stable post-solver classification of top-level callable conflicts, and the selection of each
//! source unit's Kotlin `main` entry point, which is part of that classification.

use super::super::*;
use crate::fir::DeclarationId;
use crate::program_entry::MainEntryParameters;
use std::collections::BTreeMap;

/// Classify top-level overload conflicts after the compact signature graph has finalized every
/// inferred result, and record each source unit's selected entry point in `index`. This is part of
/// Pass 1: it consumes stable declaration headers and semantic signatures only, emits diagnostics,
/// and leaves no diagnostic origin or temporary graph state for Pass 2.
pub(crate) fn finalize_streamed_top_level_conflicts(
    headers: &crate::fir::StreamedHeaderModule,
    index: &mut crate::fir::ResolvedModuleIndex,
    table: &mut SymbolTable,
    diags: &mut DiagSink,
) {
    #[derive(Clone)]
    struct Entry {
        declaration: DeclarationId,
        source: u32,
        name: String,
        signature: Signature,
        entry_point: Option<MainEntryParameters>,
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
        .entry_parameters();
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
        if let Some(span) = streamed_callable_signature_span(headers, entry.declaration) {
            index.publish_package_function_signature_span(entry.declaration, span);
        }
        let Some(mut key) =
            TopLevelFunctionConflictKey::from_signature(&entry.signature, entry.name.clone())
        else {
            continue;
        };
        let header = streamed_callable_header_by_declaration(headers, entry.declaration)
            .expect("a top-level function must retain its compact callable header");
        key.type_parameter_count = u32::try_from(header.type_parameters.len()).unwrap_or(u32::MAX);
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
                entry_point: entry.entry_point.is_some(),
            },
            &mut pending,
            &mut reserved_diagnostic_bytes,
            &mut retained_display_bytes,
        );
    }

    commit_top_level_conflict_groups(table, &groups, &pending, reserved_diagnostic_bytes, diags);
    publish_entry_points(
        index,
        entries.iter().filter_map(|entry| {
            let parameters = entry.entry_point?;
            Some((entry.source, entry.declaration, parameters))
        }),
    );
    table.conflicting_top_level_key_by_source.clear();
    for entry in entries {
        let Some(source_declaration) = entry.signature.source_decl else {
            continue;
        };
        let Some(mut key) =
            TopLevelFunctionConflictKey::from_signature(&entry.signature, entry.name)
        else {
            continue;
        };
        let header = streamed_callable_header_by_declaration(headers, entry.declaration)
            .expect("a top-level function must retain its compact callable header");
        key.type_parameter_count = u32::try_from(header.type_parameters.len()).unwrap_or(u32::MAX);
        let local = entry.signature.visibility.is_private() || entry.entry_point.is_some();
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

/// Select each source unit's entry point among its `main` candidates: `main(args)` when the unit
/// declares it, otherwise `main()`. Two candidates of one form are conflicting overloads the
/// classification above has just reported, so such a unit records no entry.
fn publish_entry_points(
    index: &mut crate::fir::ResolvedModuleIndex,
    candidates: impl Iterator<Item = (u32, DeclarationId, MainEntryParameters)>,
) {
    let mut by_source = BTreeMap::<u32, (Vec<DeclarationId>, Vec<DeclarationId>)>::new();
    for (source, declaration, parameters) in candidates {
        let (arguments, parameterless) = by_source.entry(source).or_default();
        match parameters {
            MainEntryParameters::Arguments => arguments.push(declaration),
            MainEntryParameters::None => parameterless.push(declaration),
        }
    }
    for (source, (arguments, parameterless)) in by_source {
        let (declarations, parameters) = if arguments.is_empty() {
            (parameterless, MainEntryParameters::None)
        } else {
            (arguments, MainEntryParameters::Arguments)
        };
        let [declaration] = declarations[..] else {
            continue;
        };
        let callable = index
            .callable_for_declaration(declaration)
            .expect("a finalized top-level function has a callable header")
            .id;
        index.publish_source_entry_point(
            crate::fir::SourceFileId::from_raw(source),
            crate::fir::ResolvedEntryPoint {
                callable,
                parameters,
            },
        );
    }
}
