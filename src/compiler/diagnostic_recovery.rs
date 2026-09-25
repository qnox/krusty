//! Complete source diagnostics for a module whose signature pass could not publish valid FIR.

use crate::ast::File;
use crate::diag::{DiagSink, Span};

/// Run the ordinary checker over every live declaration unit solely to recover diagnostics.
///
/// A failed signature prevents checked FIR publication, but it must not suppress independent
/// diagnostics in ordinary bodies. This path discards its AST-keyed result immediately and never
/// invokes common lowering or a backend.
pub(super) fn recover_pass_two_diagnostics(
    reparse_sources: &[crate::frontend::ReparseSource],
    symbols: &mut crate::resolve::PassTwoSymbols,
    module: crate::fir::FrontendModule,
    diags: &mut DiagSink,
) {
    let (index, _, _, _) = module.into_parts();
    for (raw_source, source) in reparse_sources.iter().enumerate() {
        if source.is_java() {
            continue;
        }
        let source_id = crate::fir::SourceFileId::from_raw(raw_source as u32);
        diags.set_file(raw_source as u32);
        let streamed_cache = crate::fir::StreamedModuleProjectionCache::default();
        let mut declaration_cursor = crate::fir::ActiveSourceCursor::new(source_id, &index);
        source.visit_diagnostic_units(diags, |active_file, diags| {
            let active = match declaration_cursor.bind_next(&active_file, source_id, &index) {
                Ok(active) => active,
                Err(error) => {
                    crate::trace_compiler!(
                        "fir",
                        "diagnostic Pass 2 could not bind declaration unit: {error:?}",
                    );
                    return;
                }
            };
            check_active_diagnostic_unit(
                &active_file,
                raw_source,
                source_id,
                &active,
                symbols,
                &index,
                &streamed_cache,
                diags,
            );
        });
    }
    // Pass 1 may already have reported a header/signature occurrence that the authoritative body
    // check sees again. Collapse across the complete compilation diagnostic stream, not merely the
    // newly appended suffix, so recovery never duplicates that exact source error.
    diags.collapse_duplicates_from(0);
    diags.sort_source_order();
    if !diags.has_errors() {
        // The recovery operation owns this postcondition. Reset attribution deliberately because
        // a prior source visit—or even a caller—may have left the sink pointing at another file.
        diags.set_file(0);
        diags.error(
            Span::new(0, 0),
            "internal error: module signatures were not finalized and no source diagnostic \
             explains it",
        );
    }
}

/// Check one invalid module's complete live declaration unit without publishing FIR or mutating
/// stable signatures. Header-only errors and ordinary-body errors are equally observable here; all
/// selection/binding state refers only to the parser arena owned by this callback.
#[allow(clippy::too_many_arguments)]
fn check_active_diagnostic_unit(
    active_file: &File,
    raw_source: usize,
    source_id: crate::fir::SourceFileId,
    active: &crate::fir::ActiveSourceDeclarations,
    symbols: &mut crate::resolve::PassTwoSymbols,
    index: &crate::fir::ResolvedModuleIndex,
    streamed_cache: &crate::fir::StreamedModuleProjectionCache,
    diags: &mut DiagSink,
) {
    let diagnostics_start = diags.diags.len();
    let declarations = index
        .source_inventory(source_id)
        .iter()
        .copied()
        .filter_map(|declaration| {
            active
                .span(active_file, declaration)
                .map(|span| (declaration, span))
        })
        .collect::<Vec<_>>();
    let selected_roots = declarations
        .iter()
        .filter(|(declaration, _)| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| anchor.owner.is_none())
        })
        .map(|(_, span)| *span)
        .collect::<std::collections::HashSet<_>>();
    let selected_bodies = declarations
        .iter()
        .map(|(_, span)| *span)
        .collect::<std::collections::HashSet<_>>();
    let selected_stable_bodies = declarations
        .iter()
        .map(|(declaration, _)| *declaration)
        .collect::<std::collections::HashSet<_>>();
    drop(crate::resolve::check_selected_declarations_in_pass_two(
        active_file,
        raw_source as u32,
        &selected_roots,
        &selected_bodies,
        active,
        &selected_stable_bodies,
        symbols,
        index,
        streamed_cache,
        diags,
    ));
    diags.collapse_duplicates_from(diagnostics_start);
}
