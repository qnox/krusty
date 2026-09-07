//! Stable post-solver classification of top-level callable conflicts.

use super::super::*;

/// Classify top-level overload conflicts after the compact signature graph has finalized every
/// inferred result. This is part of Pass 1: it consumes stable declaration headers and semantic
/// signatures only, emits diagnostics, and leaves no diagnostic origin or temporary graph state
/// for Pass 2.
pub(crate) fn finalize_streamed_top_level_conflicts(
    headers: &crate::fir::StreamedHeaderModule,
    index: &crate::fir::ResolvedModuleIndex,
    diags: &mut DiagSink,
) {
    #[derive(Clone)]
    struct Entry {
        declaration: crate::fir::DeclarationId,
        source: u32,
        package: TypeName,
        name: String,
        function: crate::libraries::FunctionInfo,
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
        let source = stub.source.raw();
        let symbols = crate::fir::StreamedModuleSymbols::for_file(index, source);
        let Some(function) = symbols.top_level_function_for_declaration(stub.id) else {
            continue;
        };
        let header = streamed_callable_header_by_declaration(headers, stub.id)
            .expect("a top-level function must retain its compact callable header");
        let params = function.semantic_params();
        let entry_point = is_kotlin_main_entry_point_shape(
            name,
            header.receiver.is_some(),
            header.type_parameters.len(),
            header.context_count,
            &params,
            function.ret.apply(function.callable.ret),
        );
        entries.push(Entry {
            declaration: stub.id,
            source,
            package: index.source_package(stub.source).unwrap_or(TypeName::ROOT),
            name: name.to_string(),
            function,
            entry_point,
        });
    }

    let mut groups = TopLevelFunctionConflictGroups::default();
    let mut pending = HashMap::new();
    let mut reserved_diagnostic_bytes = 0usize;
    let mut retained_display_bytes = 0usize;
    for entry in &entries {
        let Some(key) = TopLevelFunctionConflictKey::from_function(
            &entry.function,
            entry.package,
            entry.name.clone(),
        ) else {
            continue;
        };
        register_top_level_function_conflict(
            headers,
            &mut groups,
            TopLevelFunctionConflictRegistration {
                key,
                declaration: TopLevelFunctionConflictDecl {
                    file: entry.source,
                    declaration: entry.declaration,
                    diagnostic_span: streamed_callable_signature_span(headers, entry.declaration)
                        .expect("a top-level callable must retain its signature origin"),
                },
                private: entry.function.visibility.is_private(),
                entry_point: entry.entry_point,
            },
            &mut pending,
            &mut reserved_diagnostic_bytes,
            &mut retained_display_bytes,
        );
    }

    emit_top_level_conflict_groups(&groups, &pending, reserved_diagnostic_bytes, diags);
}
