//! Compiler orchestration.

mod metadata_handoff;
#[cfg(test)]
mod streaming_tests;

use crate::ast::File;
use crate::backend::{Artifact, Backend};
use crate::diag::{DiagSink, Span};
use crate::frontend::{check_source_set, CheckedFile, FrontendSymbols, StreamingSourceSetAnalysis};

/// Complete diagnostics for an invalid module during the normal second source pass. A failed
/// signature prevents checked FIR publication, but it must not suppress independent diagnostics in
/// ordinary bodies. The ordinary checker is still the sole semantic authority; this path only
/// discards its AST-keyed result immediately and never invokes common lowering or a backend.
fn recover_pass_two_diagnostics(
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
    let anonymous_captures = crate::resolve::discover_anonymous_object_captures_in_pass_two_file(
        active_file,
        raw_source as u32,
        &selected_roots,
        &selected_bodies,
        active,
        &selected_stable_bodies,
        symbols,
        index,
    );
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
        &anonymous_captures,
        diags,
    ));
    diags.collapse_duplicates_from(diagnostics_start);
}

/// Consume the production source-set analysis and require its finalized streaming Pass-1 product
/// before any backend lowering starts.
///
/// `FrontendModule` owns finalized headers plus retained inline/default FIR. Pass 2 receives only
/// that semantic product and owned source text; it discovers ordinary bodies from each sequentially
/// reparsed declaration unit, checks and lowers them, then drops the unit.
pub fn emit_analyzed<B: Backend>(
    analysis: impl Into<StreamingSourceSetAnalysis>,
    stems: &[String],
    backend: &B,
    module_name: &str,
    diags: &mut DiagSink,
) -> Vec<Artifact> {
    let StreamingSourceSetAnalysis {
        symbols,
        reparse_sources,
        streamed,
    } = analysis.into();
    let Some(streamed) = streamed else {
        crate::trace_compiler!(
            "fir",
            "production streaming state is unavailable after Pass 1"
        );
        if !diags.has_errors() {
            diags.error(
                Span::new(0, 0),
                "internal error: module signatures were not finalized before body lowering",
            );
        }
        return Vec::new();
    };
    let mut symbols = symbols;
    let crate::frontend::StreamedPassState {
        module,
        diagnostic_recovery,
    } = streamed;
    if diagnostic_recovery || diags.has_errors() {
        recover_pass_two_diagnostics(&reparse_sources, &mut symbols, module, diags);
        return Vec::new();
    }
    debug_assert!(
        module.index().declaration_count() >= module.index().len(),
        "every published signature must have stable declaration ownership",
    );
    if reparse_sources.len() != stems.len() {
        diags.error(
            Span::new(0, 0),
            "internal error: source files, stems, and checked types have different lengths",
        );
        return Vec::new();
    }
    if let Some(index) = reparse_sources.iter().position(|source| source.is_script()) {
        diags.set_file(index as u32);
        diags.error(
            Span::new(0, 0),
            "Kotlin scripts can be analyzed but cannot be emitted",
        );
        return Vec::new();
    }

    let (mut index, mut inline_bodies, mut default_arguments, mut source_map) = module.into_parts();
    let backend_module_facts = match crate::backend::BackendModuleFacts::from_resolved_index(&index)
    {
        Ok(facts) => facts,
        Err(error) => {
            diags.error(
                Span::new(0, 0),
                format!(
                    "internal error: cannot freeze finalized backend classifier facts: {error:?}"
                ),
            );
            return Vec::new();
        }
    };
    let mut outputs = Vec::new();
    let mut state = B::State::default();
    for (raw_source, source) in reparse_sources.iter().enumerate() {
        // Java declaration headers participated in Pass 1 through the platform provider. Java
        // executable bodies belong to javac and are never reparsed or lowered as Kotlin units.
        if source.is_java() {
            continue;
        }
        let source_id = crate::fir::SourceFileId::from_raw(raw_source as u32);
        diags.set_file(raw_source as u32);
        let streamed_cache = crate::fir::StreamedModuleProjectionCache::default();
        let mut declaration_cursor = crate::fir::ActiveSourceCursor::new(source_id, &index);
        let mut body_session = crate::fir::BodyCheckSession::default();
        for body in inline_bodies.retained_bodies_for_source(&index, source_id) {
            body_session.absorb_retained_body(body);
        }
        for body in default_arguments.retained_bodies_for_source(&index, source_id) {
            body_session.absorb_retained_body(body);
        }
        let package = source_map
            .get(source_id)
            .map(|file| file.package)
            .filter(|package| *package != crate::types::TypeName::ROOT)
            .map(|package| package.render().replace('/', "."));
        let mut ir = crate::ir::IrFile::with_package(package);
        let mut sink = match crate::fir_lower::CommonIrBodySink::new(&index, source_id, &mut ir) {
            Ok(sink) => sink,
            Err(error) => {
                diags.error(
                    Span::new(0, 0),
                    format!("internal error: cannot initialize FIR lowering: {error:?}"),
                );
                continue;
            }
        };
        metadata_handoff::attach_stable_function_inference_metadata(
            source_id,
            &index,
            sink.ir_mut(),
        );
        if let Err(error) = sink.accept_default_arguments(&index, &mut default_arguments) {
            crate::trace_compiler!("fir", "default FIR lowering failed: {error:?}");
            diags.error(
                Span::new(0, 0),
                format!("internal error: default FIR lowering failed: {error:?}"),
            );
            continue;
        }
        if let Err(error) = sink.accept_inline_bodies(&index, &mut inline_bodies) {
            diags.error(
                Span::new(0, 0),
                format!("internal error: cannot consume inline FIR: {error:?}"),
            );
            continue;
        }
        let mut source_failed = false;
        let mut source_rejected = false;
        source.visit_declaration_units(diags, |active_file, diags| {
            if source_failed {
                return;
            }
            let active = match declaration_cursor.bind_next(&active_file, source_id, &index) {
                Ok(active) => active,
                Err(error) => {
                    diags.error(
                        Span::new(0, 0),
                        format!(
                            "internal error: cannot bind sequential declaration unit: {error:?}"
                        ),
                    );
                    source_failed = true;
                    return;
                }
            };
            let work = match active.ordinary_body_work(&active_file, source_id, &index) {
                Ok(work) => work,
                Err(error) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("internal error: cannot enumerate live bodies: {error:?}"),
                    );
                    source_failed = true;
                    return;
                }
            };
            if work.is_empty() {
                return;
            }
            let groups = active_body_check_groups(work, &active_file, &active, &index);
            crate::trace_compiler!(
                "fir",
                "Pass 2 bound sequential declaration unit groups={}",
                groups.len(),
            );
            for group in groups {
                let diagnostics_start = diags.diags.len();
                if !consume_body_group(
                    &active_file,
                    &active,
                    raw_source,
                    source_id,
                    &group,
                    &mut symbols,
                    &mut index,
                    &streamed_cache,
                    &mut source_map,
                    &mut inline_bodies,
                    &mut body_session,
                    &mut sink,
                    diags,
                ) {
                    let internal_failure = diags.diags[diagnostics_start..]
                        .iter()
                        .any(|diagnostic| diagnostic.msg.starts_with("internal error:"));
                    if internal_failure {
                        source_failed = true;
                        return;
                    }
                    // A rejected ordinary body produces no FIR/IR, but it does not invalidate
                    // the stable declaration stream. Continue with later independent units so
                    // one source error cannot suppress the rest of Pass-2 diagnostics.
                    source_rejected = true;
                }
            }
            // `active_file`, its AST-keyed semantic tables, and checked FIR temporaries all
            // drop when this callback returns, before the parser resumes with the next unit.
        });
        if !source_failed && !declaration_cursor.is_finished() {
            diags.error(
                Span::new(0, 0),
                "internal error: declaration header stream was not consumed by source reparsing",
            );
            source_failed = true;
        }
        if source_failed {
            continue;
        }
        if source_rejected {
            continue;
        }
        if let Err(error) = sink.finish(&index) {
            crate::trace_compiler!("fir", "FIR lowering failed: {error:?}");
            diags.error(
                Span::new(0, 0),
                format!("internal error: FIR lowering failed: {error:?}"),
            );
            continue;
        }
        if diags.has_errors() {
            continue;
        }
        outputs.extend(backend.lower_ir_file(
            crate::backend::CheckedIrFile {
                ir,
                source: source_id,
                classifiers: crate::backend::CheckedBackendClassifiers::new(
                    &backend_module_facts,
                    symbols.semantic_platform(),
                ),
                module_name,
                stems,
            },
            &mut state,
            diags,
        ));
    }
    assert!(
        default_arguments.is_empty(),
        "Pass 2 must consume every checked signature default"
    );
    if !diags.has_errors() {
        outputs.extend(backend.finalize(state, module_name));
    }
    outputs
}

/// Stream production checked FIR through common lowering and immediately discard each completed IR
/// file. This is the lowering-only conformance boundary: callers do not construct a target backend,
/// and target realization or emission cannot influence the result.
pub fn lower_analyzed_to_common_ir(
    analysis: impl Into<StreamingSourceSetAnalysis>,
    stems: &[String],
    module_name: &str,
    diags: &mut DiagSink,
) {
    struct DiscardCommonIr;

    impl Backend for DiscardCommonIr {
        type State = ();

        fn lower_file(
            &self,
            _checked: CheckedFile<'_>,
            _stem: &str,
            _state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            panic!("common-lowering census accepts streamed checked FIR only")
        }

        fn lower_ir_file(
            &self,
            _file: crate::backend::CheckedIrFile<'_>,
            _state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            Vec::new()
        }

        fn finalize(&self, _state: Self::State, _module_name: &str) -> Vec<Artifact> {
            Vec::new()
        }
    }

    let outputs = emit_analyzed(analysis, stems, &DiscardCommonIr, module_name, diags);
    assert!(
        outputs.is_empty(),
        "the common-lowering census must not produce target artifacts"
    );
}

#[derive(Debug)]
struct BodyCheckGroup {
    root: crate::fir::DeclarationId,
    bodies: std::collections::HashSet<crate::fir::DeclarationId>,
    work: Vec<crate::fir::BodyWorkItem>,
}

/// Partition one active source's stable work by the parser declaration subtree needed to recreate
/// its lexical scopes. Named classifiers are independently reparsed roots; local/anonymous
/// classifiers remain with the enclosing callable that introduces them.
fn body_check_groups(
    work: Vec<(crate::fir::DeclarationId, crate::fir::BodyWorkItem)>,
) -> Vec<BodyCheckGroup> {
    let mut groups = Vec::<(crate::fir::DeclarationId, BodyCheckGroup)>::new();
    for (root, unit) in work {
        crate::trace_compiler!(
            "fir",
            "Pass 2 group body={:?} kind={:?} root={root:?}",
            unit.declaration,
            unit.kind,
        );
        let position = groups
            .iter()
            .position(|(candidate, _)| *candidate == root)
            .unwrap_or_else(|| {
                groups.push((
                    root,
                    BodyCheckGroup {
                        root,
                        bodies: std::collections::HashSet::new(),
                        work: Vec::new(),
                    },
                ));
                groups.len() - 1
            });
        let group = &mut groups[position].1;
        group.bodies.insert(unit.declaration);
        group.work.push(unit);
    }
    groups.into_iter().map(|(_, group)| group).collect()
}

/// Reconstruct checker roots from the one parser unit that is live now. Source containment is
/// consulted only inside that unit to repair parser-hoisted local classifiers; no resulting root
/// survives the callback.
fn active_body_check_groups(
    mut work: Vec<crate::fir::BodyWorkItem>,
    file: &File,
    active: &crate::fir::ActiveSourceDeclarations,
    index: &crate::fir::ResolvedModuleIndex,
) -> Vec<BodyCheckGroup> {
    fn ordinary_root(
        mut declaration: crate::fir::DeclarationId,
        index: &crate::fir::ResolvedModuleIndex,
    ) -> crate::fir::DeclarationId {
        loop {
            let Some(anchor) = index.declaration_anchor(declaration) else {
                return declaration;
            };
            let local_classifier = anchor.kind == crate::fir::DeclarationKind::Classifier
                && index.declaration_header(declaration).is_some_and(|header| {
                    header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                });
            if (anchor.kind == crate::fir::DeclarationKind::Classifier && !local_classifier)
                || anchor.owner.is_none()
            {
                return declaration;
            }
            declaration = anchor.owner.expect("a non-root declaration has an owner");
        }
    }

    // Parser hoisting may enumerate a local classifier before the executable declaration that
    // creates it. Check by the live unit's lexical order so the enclosing body publishes capture
    // state before a nested member consumes it. This ordering is computed from the active AST and
    // disappears with it; no Pass-1 coordinate participates.
    work.sort_by_key(|unit| {
        active
            .span(file, unit.declaration)
            .map_or((u32::MAX, u32::MAX, unit.declaration), |span| {
                (span.lo, span.hi, unit.declaration)
            })
    });
    let declarations = work.iter().map(|unit| unit.declaration).collect::<Vec<_>>();
    let mut rooted = Vec::with_capacity(work.len());
    for unit in work {
        let mut root = ordinary_root(unit.declaration, index);
        let root_is_orphaned_local_classifier =
            index.declaration_anchor(root).is_some_and(|anchor| {
                anchor.kind == crate::fir::DeclarationKind::Classifier
                    && anchor.owner.is_none()
                    && index.declaration_header(root).is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    })
            });
        if root_is_orphaned_local_classifier {
            if let Some(classifier_span) = active.span(file, root) {
                if let Some(enclosing) = declarations
                    .iter()
                    .copied()
                    .filter(|candidate| *candidate != root)
                    .filter_map(|candidate| {
                        active.span(file, candidate).map(|span| (candidate, span))
                    })
                    .filter(|(_, span)| {
                        span.lo <= classifier_span.lo
                            && classifier_span.hi <= span.hi
                            && *span != classifier_span
                    })
                    .min_by_key(|(_, span)| span.hi - span.lo)
                    .map(|(candidate, _)| candidate)
                {
                    root = ordinary_root(enclosing, index);
                }
            }
        }
        rooted.push((root, unit));
    }
    body_check_groups(rooted)
}

fn check_body_group(
    active_file: &File,
    raw_source: usize,
    source_id: crate::fir::SourceFileId,
    group: &BodyCheckGroup,
    active: &crate::fir::ActiveSourceDeclarations,
    symbols: &mut crate::resolve::PassTwoSymbols,
    index: &mut crate::fir::ResolvedModuleIndex,
    streamed_cache: &crate::fir::StreamedModuleProjectionCache,
    diags: &mut DiagSink,
) -> Option<crate::resolve::TypeInfo> {
    let diagnostics_start = diags.diags.len();
    let selected_roots = std::collections::HashSet::from([active.span(active_file, group.root)?]);
    let body_spans = group
        .bodies
        .iter()
        .filter_map(|declaration| active.span(active_file, *declaration))
        .collect::<Vec<_>>();
    if body_spans.len() != group.bodies.len() {
        let missing = group
            .bodies
            .iter()
            .filter(|declaration| active.span(active_file, **declaration).is_none())
            .copied()
            .collect::<Vec<_>>();
        crate::trace_compiler!(
            "fir",
            "active body declarations without parser bindings root={:?} missing={missing:?}",
            group.root,
        );
        diags.error(
            Span::new(0, 0),
            "internal error: active body declaration has no parser binding",
        );
        return None;
    }
    // A constructor, initializer, accessor, and nested anonymous member can legitimately share the
    // same enclosing expression span. The checker selects syntax regions, so deduplicate only after
    // proving that every stable body declaration has an active parser binding.
    let mut selected_bodies = body_spans
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    if selected_bodies.len() != group.bodies.len() {
        crate::trace_compiler!(
            "fir",
            "active body declarations share parser spans root={:?} bindings={:?}",
            group.root,
            group
                .bodies
                .iter()
                .map(|declaration| (*declaration, active.span(active_file, *declaration)))
                .collect::<Vec<_>>(),
        );
    }
    // A local declaration's stable owner chain names the executable declarations whose lexical
    // scopes introduce it. Re-enter those ancestor bodies during this active reparse so nested
    // class headers, captures, and local bindings are checked on their real tower rungs. They are
    // traversal-only: `group.work` still contains only the requested ordinary FIR bodies, and these
    // transient parser spans disappear with the callback.
    for &body in &group.bodies {
        let mut current = body;
        while let Some(owner) = index
            .declaration_header(current)
            .and_then(|header| header.owner)
            .or_else(|| {
                index
                    .declaration_anchor(current)
                    .and_then(|anchor| anchor.owner)
            })
        {
            if owner == group.root {
                if !index
                    .declaration_header(owner)
                    .is_some_and(|header| header.kind == crate::fir::DeclarationKind::Classifier)
                {
                    if let Some(span) = active.span(active_file, owner) {
                        selected_bodies.insert(span);
                    }
                }
                break;
            }
            if let Some(span) = active.span(active_file, owner) {
                selected_bodies.insert(span);
            }
            current = owner;
        }
    }
    let anonymous_captures = crate::resolve::discover_anonymous_object_captures_in_pass_two_file(
        active_file,
        raw_source as u32,
        &selected_roots,
        &selected_bodies,
        active,
        &group.bodies,
        symbols,
        index,
    );
    let info = crate::resolve::check_selected_declarations_in_pass_two(
        active_file,
        raw_source as u32,
        &selected_roots,
        &selected_bodies,
        active,
        &group.bodies,
        symbols,
        index,
        streamed_cache,
        &anonymous_captures,
        diags,
    );
    // Capture discovery and the authoritative body check enter the same active lexical headers.
    // Keep one exact source diagnostic when both observe the same invalid declaration.
    diags.collapse_duplicates_from(diagnostics_start);
    if diags.diags[diagnostics_start..]
        .iter()
        .any(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
    {
        return None;
    }
    if let Err(declarations) = crate::resolve::publish_checked_local_signatures_in_pass_two_root(
        active_file,
        active,
        source_id,
        symbols.semantic_platform(),
        &info,
        index,
        group.root,
        &group.bodies,
    ) {
        diags.error(
            Span::new(0, 0),
            format!(
                "internal error: checked local signatures were not publishable: {declarations:?}"
            ),
        );
        return None;
    }
    Some(info)
}

#[derive(Default)]
struct DeferredCheckedBodies(Vec<(crate::fir::BodyOwnerId, crate::fir::FirBody)>);

impl crate::fir::CheckedBodySink for DeferredCheckedBodies {
    fn accept_finalized(&mut self, owner: crate::fir::BodyOwnerId, body: crate::fir::FirBody) {
        self.0.push((owner, body));
    }
}

#[allow(clippy::too_many_arguments)]
fn consume_body_group(
    active_file: &File,
    active: &crate::fir::ActiveSourceDeclarations,
    raw_source: usize,
    source_id: crate::fir::SourceFileId,
    group: &BodyCheckGroup,
    symbols: &mut crate::resolve::PassTwoSymbols,
    index: &mut crate::fir::ResolvedModuleIndex,
    streamed_cache: &crate::fir::StreamedModuleProjectionCache,
    source_map: &mut crate::fir::SourceMap,
    inline_bodies: &mut crate::fir::InlineBodyStore,
    body_session: &mut crate::fir::BodyCheckSession,
    sink: &mut crate::fir_lower::CommonIrBodySink<'_>,
    diags: &mut DiagSink,
) -> bool {
    let Some(info) = check_body_group(
        active_file,
        raw_source,
        source_id,
        group,
        active,
        symbols,
        index,
        streamed_cache,
        diags,
    ) else {
        return false;
    };
    if let Err(error) = sink.refresh_body_local_declarations(index) {
        diags.error(
            Span::new(0, 0),
            format!("internal error: cannot publish body-local IR declarations: {error:?}"),
        );
        return false;
    }
    for body in group.work.iter().copied() {
        let mut checked = DeferredCheckedBodies::default();
        let result = crate::fir::check_and_dispatch_active_body_in_session(
            active_file,
            active,
            &info,
            source_id,
            body,
            index,
            source_map.origins_mut(),
            inline_bodies,
            &mut checked,
            body_session,
        );
        let Err(error) = result else {
            for (owner, body) in checked.0 {
                if let Err(error) = sink.accept_streamed_body(index, inline_bodies, owner, body) {
                    diags.error(
                        Span::new(0, 0),
                        format!("internal error: checked FIR lowering failed: {error:?}"),
                    );
                    return false;
                }
            }
            continue;
        };
        crate::trace_compiler!("fir", "checked FIR construction failed: {error:?}");
        if let crate::fir::CheckedBodyDriverFailure::Check(failure) = &error {
            if let Some(span) = failure.span {
                if let Some((expression, _)) = active_file
                    .expr_spans
                    .iter()
                    .enumerate()
                    .find(|(_, candidate)| **candidate == span)
                {
                    crate::trace_compiler!(
                        "fir",
                        "failed AST expression {expression}: {:?}",
                        active_file.expr(crate::ast::ExprId(expression as u32)),
                    );
                    for (nested, nested_span) in active_file.expr_spans.iter().enumerate() {
                        if nested != expression
                            && nested_span.lo >= span.lo
                            && nested_span.hi <= span.hi
                        {
                            crate::trace_compiler!(
                                "fir",
                                "nested AST expression {nested}: {:?}",
                                active_file.expr(crate::ast::ExprId(nested as u32)),
                            );
                        }
                    }
                }
            }
        }
        match &error {
            crate::fir::CheckedBodyDriverFailure::Check(failure)
                if failure.kind
                    == crate::fir::BodyCheckFailureKind::LocalVariableCallableReference =>
            {
                diags.error(
                    failure.span.unwrap_or_else(|| Span::new(0, 0)),
                    "references to variables aren't supported yet",
                );
            }
            _ => diags.error(
                Span::new(0, 0),
                format!("internal error: checked FIR construction failed: {error:?}"),
            ),
        }
        return false;
    }
    metadata_handoff::attach_checked_declaration_metadata(
        active_file,
        active,
        &info,
        source_id,
        group.root,
        index,
        sink.ir_mut(),
    );
    true
}

/// Which Pass-1/Pass-2 step refused a declaration in a frontend-only census run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontendStage {
    /// Pass-1 signature solving never published a pending-free index for the module.
    Signatures,
    /// The active file was rejected before any body could be checked (lex, parse, or check).
    Check,
    /// Checked local signatures could not be published into the stable index.
    LocalSignatures,
    /// AST-to-FIR body checking refused a scheduled body unit.
    FirCheck,
}

impl FrontendStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Signatures => "signatures",
            Self::Check => "check",
            Self::LocalSignatures => "local-signatures",
            Self::FirCheck => "fir-check",
        }
    }
}

/// One frontend refusal, identified without any backend participation.
#[derive(Clone, Debug)]
pub struct FrontendFailure {
    pub stage: FrontendStage,
    pub source: u32,
    pub span: Option<Span>,
    /// Stable classifier for histogramming — a `BodyCheckFailureKind`-style discriminant, not a
    /// user-facing message.
    pub kind: String,
    pub detail: String,
}

/// The result of streaming a source set through the production front end with no backend attached.
#[derive(Clone, Debug, Default)]
pub struct FrontendCensus {
    /// Ordinary body units that produced checked FIR.
    pub bodies: usize,
    pub failures: Vec<FrontendFailure>,
}

impl FrontendCensus {
    pub fn is_conformant(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Every checked body is dropped immediately: a frontend census measures whether checked FIR can be
/// CONSTRUCTED, so nothing downstream of construction may influence the result.
struct DiscardCheckedBodies {
    accepted: usize,
}

impl crate::fir::CheckedBodySink for DiscardCheckedBodies {
    fn accept_finalized(&mut self, _owner: crate::fir::BodyOwnerId, _body: crate::fir::FirBody) {
        self.accepted += 1;
    }
}

/// Stream a source set through the production two-pass front end and report every frontend refusal,
/// with no backend attached.
///
/// This is the measurement instrument for frontend conformance: `fir_lower`, the JVM realization
/// passes, and emission are never constructed, so a backend gap cannot appear in the result and a
/// backend refusal cannot mask a frontend one. Diagnostics reported by the front end itself are
/// counted as refusals too — for a source the reference compiler accepts, a diagnostic IS a
/// conformance failure.
pub fn check_frontend_only(
    analysis: impl Into<StreamingSourceSetAnalysis>,
    diags: &mut DiagSink,
) -> FrontendCensus {
    let StreamingSourceSetAnalysis {
        mut symbols,
        reparse_sources,
        streamed,
    } = analysis.into();
    let mut census = FrontendCensus::default();
    // A diagnostic the front end has ALREADY reported is the primary failure. Signature
    // finalization also fails whenever an ordinary diagnostic made a signature unsolvable, so
    // reporting the missing streamed index first would file every rejected source under the
    // signature solver and hide the real cause.
    let first_error = diags
        .diags
        .iter()
        .find(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
        .map(|diagnostic| (diagnostic.file, diagnostic.span, diagnostic.msg.clone()));
    if first_error.is_some() {
        // A failed stable signature cannot enter checked FIR, but it also must not prevent the
        // normal second source pass from finding independent body diagnostics. Consume the compact
        // partial module exactly as emission does, and discard every reparsed unit immediately;
        // this is recovery inside Pass 2, not another source pass or a second checker.
        if let Some(streamed) = streamed {
            recover_pass_two_diagnostics(&reparse_sources, &mut symbols, streamed.module, diags);
        }
        let (source, span, message) = diags
            .diags
            .iter()
            .find(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            .map(|diagnostic| (diagnostic.file, diagnostic.span, diagnostic.msg.clone()))
            .expect("the preexisting frontend error remains after diagnostic recovery");
        census.failures.push(FrontendFailure {
            stage: if message.starts_with("internal error:") {
                FrontendStage::Signatures
            } else {
                FrontendStage::Check
            },
            source,
            span: Some(span),
            kind: if message.starts_with("internal error:") {
                "Internal".to_string()
            } else {
                "Rejected".to_string()
            },
            detail: message,
        });
        return census;
    }
    let Some(streamed) = streamed else {
        census.failures.push(FrontendFailure {
            stage: FrontendStage::Signatures,
            source: 0,
            span: None,
            kind: "NotFinalized".to_string(),
            detail: "module signatures were not finalized before body streaming".to_string(),
        });
        return census;
    };
    let mut symbols = symbols;
    let crate::frontend::StreamedPassState {
        module,
        diagnostic_recovery,
    } = streamed;
    if diagnostic_recovery {
        census.failures.push(FrontendFailure {
            stage: FrontendStage::Signatures,
            source: 0,
            span: None,
            kind: "MissingSignatureDiagnostic".to_string(),
            detail: "signature recovery module had no reported source diagnostic".to_string(),
        });
        return census;
    }
    let (mut index, mut inline_bodies, mut default_arguments, mut source_map) = module.into_parts();

    for (raw_source, source) in reparse_sources.iter().enumerate() {
        if source.is_java() {
            continue;
        }
        if source.is_script() {
            // Scripts are not JVM emission units yet, but their executable body still belongs to
            // frontend conformance. Reparse and check it in Pass 2 against the finalized module;
            // do not retain its AST or invent a script FIR/backend path merely to obtain diagnostics.
            let source_id = crate::fir::SourceFileId::from_raw(raw_source as u32);
            diags.set_file(raw_source as u32);
            let diagnostics_start = diags.diags.len();
            let streamed_cache = crate::fir::StreamedModuleProjectionCache::default();
            source.visit_diagnostic_units(diags, |active_file, diags| {
                let mut cursor = crate::fir::ActiveSourceCursor::new(source_id, &index);
                let active = match cursor.bind_next(&active_file, source_id, &index) {
                    Ok(active) if cursor.is_finished() => active,
                    Ok(_) | Err(_) => {
                        diags.error(
                            Span::new(0, 0),
                            "internal error: script declarations did not bind to the stable module",
                        );
                        return;
                    }
                };
                let selected_spans = std::collections::HashSet::new();
                let selected_bodies = std::collections::HashSet::new();
                drop(crate::resolve::check_selected_declarations_in_pass_two(
                    &active_file,
                    raw_source as u32,
                    &selected_spans,
                    &selected_spans,
                    &active,
                    &selected_bodies,
                    &mut symbols,
                    &index,
                    &streamed_cache,
                    &std::collections::HashMap::new(),
                    diags,
                ));
            });
            let reported = diags.diags[diagnostics_start..]
                .iter()
                .find(|diagnostic| diagnostic.severity == crate::diag::Severity::Error);
            census.failures.push(FrontendFailure {
                stage: FrontendStage::Check,
                source: raw_source as u32,
                span: reported.map(|diagnostic| diagnostic.span),
                kind: if reported.is_some() {
                    "Rejected".to_string()
                } else {
                    "UnsupportedScript".to_string()
                },
                detail: reported.map_or_else(
                    || "Kotlin scripts are outside the production JVM body stream".to_string(),
                    |diagnostic| diagnostic.msg.clone(),
                ),
            });
            continue;
        }
        let source_id = crate::fir::SourceFileId::from_raw(raw_source as u32);
        diags.set_file(raw_source as u32);
        let streamed_cache = crate::fir::StreamedModuleProjectionCache::default();
        let mut declaration_cursor = crate::fir::ActiveSourceCursor::new(source_id, &index);
        let mut sink = DiscardCheckedBodies { accepted: 0 };
        let mut body_session = crate::fir::BodyCheckSession::default();
        for body in inline_bodies.retained_bodies_for_source(&index, source_id) {
            body_session.absorb_retained_body(body);
        }
        for body in default_arguments.retained_bodies_for_source(&index, source_id) {
            body_session.absorb_retained_body(body);
        }
        let source_diagnostic_start = diags.diags.len();
        let mut source_failed = false;
        source.visit_declaration_units(diags, |active_file, diags| {
            if source_failed {
                return;
            }
            let active = match declaration_cursor.bind_next(&active_file, source_id, &index) {
                Ok(active) => active,
                Err(error) => {
                    census.failures.push(FrontendFailure {
                        stage: FrontendStage::LocalSignatures,
                        source: raw_source as u32,
                        span: None,
                        kind: "DeclarationBinding".to_string(),
                        detail: format!("{error:?}"),
                    });
                    source_failed = true;
                    return;
                }
            };
            let work = match active.ordinary_body_work(&active_file, source_id, &index) {
                Ok(work) => work,
                Err(error) => {
                    census.failures.push(FrontendFailure {
                        stage: FrontendStage::LocalSignatures,
                        source: raw_source as u32,
                        span: None,
                        kind: "BodyEnumeration".to_string(),
                        detail: format!("{error:?}"),
                    });
                    source_failed = true;
                    return;
                }
            };
            for group in active_body_check_groups(work, &active_file, &active, &index) {
                let before = diags.diags.len();
                let Some(info) = check_body_group(
                    &active_file,
                    raw_source,
                    source_id,
                    &group,
                    &active,
                    &mut symbols,
                    &mut index,
                    &streamed_cache,
                    diags,
                ) else {
                    let reported = diags.diags[before..]
                        .iter()
                        .find(|diagnostic| diagnostic.severity == crate::diag::Severity::Error);
                    let internal = reported
                        .is_some_and(|diagnostic| diagnostic.msg.starts_with("internal error:"));
                    census.failures.push(FrontendFailure {
                        stage: if internal {
                            FrontendStage::LocalSignatures
                        } else {
                            FrontendStage::Check
                        },
                        source: raw_source as u32,
                        span: reported.map(|diagnostic| diagnostic.span),
                        kind: "Rejected".to_string(),
                        detail: reported
                            .map(|diagnostic| diagnostic.msg.clone())
                            .unwrap_or_default(),
                    });
                    source_failed = true;
                    return;
                };
                for body in group.work {
                    if let Err(error) = crate::fir::check_and_dispatch_active_body_in_session(
                        &active_file,
                        &active,
                        &info,
                        source_id,
                        body,
                        &index,
                        source_map.origins_mut(),
                        &mut inline_bodies,
                        &mut sink,
                        &mut body_session,
                    ) {
                        census.failures.push(FrontendFailure {
                            stage: FrontendStage::FirCheck,
                            source: raw_source as u32,
                            span: match &error {
                                crate::fir::CheckedBodyDriverFailure::Check(failure) => {
                                    failure.span
                                }
                                _ => None,
                            },
                            kind: match &error {
                                crate::fir::CheckedBodyDriverFailure::Check(failure) => {
                                    format!("{:?}", failure.kind)
                                }
                                other => format!("{other:?}"),
                            },
                            detail: format!("{error:?}"),
                        });
                    }
                }
            }
        });
        if !source_failed && !declaration_cursor.is_finished() {
            census.failures.push(FrontendFailure {
                stage: FrontendStage::LocalSignatures,
                source: raw_source as u32,
                span: None,
                kind: "UnconsumedHeaders".to_string(),
                detail: "declaration header stream was not consumed by source reparsing"
                    .to_string(),
            });
            source_failed = true;
        }
        if !source_failed {
            if let Some(reported) = diags.diags[source_diagnostic_start..]
                .iter()
                .find(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
            {
                census.failures.push(FrontendFailure {
                    stage: FrontendStage::Check,
                    source: raw_source as u32,
                    span: Some(reported.span),
                    kind: "Rejected".to_string(),
                    detail: reported.msg.clone(),
                });
            }
        }
        for (_, body) in default_arguments.take_for_source(&index, source_id) {
            let owner = body.owner();
            crate::fir::CheckedBodySink::accept(&mut sink, owner, body);
        }
        diags.set_file(raw_source as u32);
        census.bodies += sink.accepted;
    }
    assert!(default_arguments.is_empty());
    census
}

/// Check each parsed file and hand it to the backend.
pub fn compile<B: Backend>(
    files: &[File],
    stems: &[String],
    syms: &mut FrontendSymbols,
    backend: &B,
    module_name: &str,
    diags: &mut DiagSink,
) -> Vec<Artifact> {
    let types = check_source_set(files, syms, diags);
    emit_checked(files, stems, &types, syms, backend, module_name, diags)
}

/// Hand a checked source set to a backend.
pub fn emit_checked<B: Backend>(
    files: &[File],
    stems: &[String],
    types: &[Option<crate::frontend::FrontendTypeInfo>],
    syms: &FrontendSymbols,
    backend: &B,
    module_name: &str,
    diags: &mut DiagSink,
) -> Vec<Artifact> {
    if files.len() != stems.len() || files.len() != types.len() {
        diags.error(
            Span::new(0, 0),
            "internal error: source files, stems, and checked types have different lengths",
        );
        return Vec::new();
    }
    if let Some(index) = files.iter().position(|file| file.is_script) {
        diags.set_file(index as u32);
        diags.error(
            Span::new(0, 0),
            "Kotlin scripts can be analyzed but cannot be emitted",
        );
        return Vec::new();
    }
    let mut outputs = Vec::new();
    let mut state = B::State::default();
    for (i, ((file, stem), info)) in files.iter().zip(stems).zip(types).enumerate() {
        diags.set_file(i as u32);
        let Some(info) = info.as_ref() else {
            continue;
        };
        if diags.has_errors() {
            continue;
        }
        outputs.extend(backend.lower_file(
            CheckedFile {
                file,
                file_index: i as u32,
                info,
                symbols: syms,
                module_name,
            },
            stem,
            &mut state,
            diags,
        ));
    }
    if !diags.has_errors() {
        outputs.extend(backend.finalize(state, module_name));
    }
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Artifact;
    use crate::features::LangFeatures;
    use crate::frontend::{
        analyze_source_set_with_features, collect_signatures, parse_source_with_detected_features,
    };
    use crate::lexer::lex;
    use crate::libraries::EmptySymbolSource;
    use crate::parser::parse_script_with_features;
    use crate::source::SourceInput;
    use crate::types::Ty;

    struct RecordingBackend;

    struct ModuleCallRecordingBackend;

    struct FileAnnotationRecordingBackend;

    struct MemberAnnotationRecordingBackend;

    struct EnumEntryCallRecordingBackend;

    impl Backend for RecordingBackend {
        type State = usize;

        fn lower_file(
            &self,
            checked: CheckedFile<'_>,
            stem: &str,
            state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            *state += checked.file.decls.len();
            vec![(format!("{stem}.out"), Vec::new())]
        }

        fn lower_ir_file(
            &self,
            file: crate::backend::CheckedIrFile<'_>,
            state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            *state += file
                .ir
                .functions
                .iter()
                .filter(|function| function.body.is_some())
                .count();
            let stem = &file.stems[file.source.raw() as usize];
            vec![(format!("{stem}.out"), Vec::new())]
        }

        fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
            vec![("module.out".to_string(), state.to_string().into_bytes())]
        }
    }

    impl Backend for ModuleCallRecordingBackend {
        type State = usize;

        fn lower_file(
            &self,
            _checked: CheckedFile<'_>,
            _stem: &str,
            _state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            panic!("streamed production emission must not invoke legacy syntax lowering")
        }

        fn lower_ir_file(
            &self,
            file: crate::backend::CheckedIrFile<'_>,
            state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            *state += file
                .ir
                .exprs
                .iter()
                .filter(|expression| {
                    matches!(
                        expression,
                        crate::ir::IrExpr::Call {
                            callee: crate::ir::Callee::Module { .. },
                            ..
                        }
                    )
                })
                .count();
            Vec::new()
        }

        fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
            vec![(
                "module-calls.out".to_string(),
                state.to_string().into_bytes(),
            )]
        }
    }

    impl Backend for FileAnnotationRecordingBackend {
        type State = usize;

        fn lower_file(
            &self,
            _checked: CheckedFile<'_>,
            _stem: &str,
            _state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            panic!("streamed production emission must not invoke legacy syntax lowering")
        }

        fn lower_ir_file(
            &self,
            file: crate::backend::CheckedIrFile<'_>,
            state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            *state += file.ir.file_annotations.len();
            Vec::new()
        }

        fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
            vec![(
                "file-annotations.out".to_string(),
                state.to_string().into_bytes(),
            )]
        }
    }

    impl Backend for MemberAnnotationRecordingBackend {
        type State = usize;

        fn lower_file(
            &self,
            _checked: CheckedFile<'_>,
            _stem: &str,
            _state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            panic!("streamed production emission must not invoke legacy syntax lowering")
        }

        fn lower_ir_file(
            &self,
            file: crate::backend::CheckedIrFile<'_>,
            state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            if file.ir.function_annotations.values().any(|annotations| {
                annotations
                    .applications()
                    .any(|annotation| annotation.internal.matches("Marker"))
            }) {
                *state |= 1;
            }
            if file.ir.fn_param_annotations.values().any(|parameters| {
                parameters.iter().any(|annotations| {
                    annotations
                        .applications()
                        .any(|annotation| annotation.internal.matches("ParameterMarker"))
                })
            }) {
                *state |= 2;
            }
            Vec::new()
        }

        fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
            vec![(
                "member-annotations.out".to_string(),
                state.to_string().into_bytes(),
            )]
        }
    }

    impl Backend for EnumEntryCallRecordingBackend {
        type State = usize;

        fn lower_file(
            &self,
            _checked: CheckedFile<'_>,
            _stem: &str,
            _state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            panic!("streamed production emission must not invoke legacy syntax lowering")
        }

        fn lower_ir_file(
            &self,
            file: crate::backend::CheckedIrFile<'_>,
            state: &mut Self::State,
            _diags: &mut DiagSink,
        ) -> Vec<Artifact> {
            if file
                .ir
                .classes
                .iter()
                .any(|class| class.fq_name.matches("Foo$FOO"))
            {
                *state |= 1;
            }
            if file
                .ir
                .exprs
                .iter()
                .any(|expression| matches!(expression, crate::ir::IrExpr::MethodCall { .. }))
            {
                *state |= 2;
            }
            if file
                .ir
                .exprs
                .iter()
                .all(|expression| !matches!(expression, crate::ir::IrExpr::Checked(_)))
            {
                *state |= 4;
            }
            Vec::new()
        }

        fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
            vec![(
                "enum-entry-calls.out".to_string(),
                state.to_string().into_bytes(),
            )]
        }
    }

    #[test]
    fn inline_local_class_capture_context_survives_the_pass_two_reparse() {
        let inputs = [SourceInput::kotlin(
            r#"inline fun <T> once(block: () -> T): T = block()
               inline fun host(crossinline callback: () -> Int): Int {
                   var outside = 0
                   return once {
                       var inside = 0
                       val holder = object {
                           fun count() { outside++; inside++ }
                           fun value(): Int = callback()
                       }
                       holder.count()
                       holder.value() + outside + inside
                   }
               }"#,
        )
        .with_file_stem("InlineLocalClassCapture")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(
            census.is_conformant(),
            "Pass-2 nested bodies must receive Pass-1 checked capture context: {:?}",
            census.failures
        );
    }

    #[test]
    fn anonymous_object_captures_outer_receiver_used_inside_a_local_function() {
        let inputs = [SourceInput::kotlin(
            r#"class Host {
                   fun outer() {
                       val instance = object {
                           fun invoke() {
                               fun nested() { target() }
                               nested()
                           }
                       }
                       instance.invoke()
                   }
                   fun target() {}
               }"#,
        )
        .with_file_stem("AnonymousOuterReceiver")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn anonymous_method_demands_later_inferred_member_in_the_same_pass_two_group() {
        let inputs = [SourceInput::kotlin(
            r#"fun box(): String {
                   val prefix = "a"
                   val value = object {
                       override fun toString(): String = foo(prefix) + foo("b")
                       fun foo(value: String) = value + value
                   }
                   return value.toString()
               }"#,
        )
        .with_file_stem("AnonymousForwardMember")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn production_frontend_selects_applied_concrete_super_property_over_abstract_builtin() {
        let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() else {
            return;
        };
        let inputs = [SourceInput::kotlin(
            r#"interface DefaultSize<T> {
                   val size: T get() = 56 as T
               }
               class Values : Collection<String>, DefaultSize<Int> {
                   override fun isEmpty() = throw UnsupportedOperationException()
                   override fun contains(value: String) = throw UnsupportedOperationException()
                   override fun iterator() = throw UnsupportedOperationException()
                   override fun containsAll(values: Collection<String>) = throw UnsupportedOperationException()
                   override val size: Int get() = super.size
               }"#,
        )
        .with_file_stem("AppliedSuperProperty")];
        let mut paths = vec![stdlib];
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn bounded_local_classifier_does_not_steal_nested_classifier_identity() {
        let inputs = [SourceInput::kotlin(
            r#"class BeforeA
               class BeforeB
               class Outer {
                   class NestedA
                   class NestedB
                   fun make(): Any {
                       class Local
                       return Local()
                   }
               }"#,
        )
        .with_file_stem("BoundedLocalClassifierIdentity")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn bounded_local_constructor_does_not_republish_top_level_constructor_shape() {
        let inputs = [SourceInput::kotlin(
            r#"class Box<T>(val value: T)
               fun <F> enterLocal(box: Box<F>) {
                   class Local<L>(value: L)
                   Local(box.value)
               }
               fun box(): String = Box("OK").value"#,
        )
        .with_file_stem("BoundedLocalConstructorIdentity")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn bounded_top_level_unit_property_is_visible_to_later_units() {
        let inputs = [SourceInput::kotlin(
            r#"val first: Unit get() {}
               val second = first
               fun use(): Unit {
                   first
                   second
               }"#,
        )
        .with_file_stem("TopLevelUnitProperty")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn pass_two_checks_inferred_extension_body_against_its_stable_result() {
        let inputs = [SourceInput::kotlin(
            r#"class Wrap<T>(val value: T)
               interface Consumer<T> { fun consume(value: T): String }
               open class Base<T> : Consumer<Wrap<T>> {
                   override fun consume(value: Wrap<T>): String = "OK"
               }
               class Derived : Base<String>()
               fun <T> Consumer<Wrap<T>>.adapt(value: T) = consume(Wrap(value))
               fun box(): String = Derived().adapt("OK")"#,
        )
        .with_file_stem("StableExtensionResult")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn covariant_override_keeps_inherited_default_provider_and_derived_result() {
        let inputs = [SourceInput::kotlin(
            r#"open class Base {
                   open fun value(input: Any = "OK"): Any = input
               }
               class Derived : Base() {
                   override fun value(input: Any): String = "OK"
               }
               fun box(): String = Derived().value()"#,
        )
        .with_file_stem("InheritedOverrideDefault")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert_eq!(
            analysis
                .streamed
                .as_ref()
                .expect("the two-pass frontend must finalize")
                .module
                .default_arguments()
                .len(),
            2,
            "the base declaration and its overriding call target share checked default payload"
        );
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn anonymous_super_constructor_uses_canonical_active_classifier() {
        let inputs = [SourceInput::kotlin(
            r#"abstract class Base {
                   abstract fun value(): String
               }
               fun box(): String {
                   var result = "fail"
                   val instance = object : Base() {
                       override fun value(): String {
                           result = "OK"
                           return result
                       }
                   }
                   return instance.value()
               }"#,
        )
        .with_file_stem("CanonicalAnonymousClassifier")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn anonymous_member_signature_keeps_outer_type_parameter_identity() {
        let inputs = [SourceInput::kotlin(
            r#"interface Cursor<T>
               interface Stream<T> {
                   fun iterator(): Cursor<T>
               }
               class ZippingStream<T1, T2>(
                   val stream1: Stream<T1>,
                   val stream2: Stream<T2>,
               ) {
                   fun iterator(): Any = object {
                       val iterator1 = stream1.iterator()
                       val iterator2 = stream2.iterator()
                   }
               }
               object EmptyCursor : Cursor<Nothing>
               object EmptyStream : Stream<Nothing> {
                   override fun iterator(): Cursor<Nothing> = EmptyCursor
               }
               fun consume() {
                   ZippingStream(EmptyStream, EmptyStream)
               }"#,
        )
        .with_file_stem("AnonymousOuterTypeParameterIdentity")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn nested_member_of_local_class_keeps_classifier_owner_and_captures() {
        let inputs = [SourceInput::kotlin(
            r#"open class Base(val read: () -> String)
               fun box(): String {
                   val prefix = "P"
                   class Local(val value: String) {
                       inner class Inner : Base({ value }) {
                           fun current(): String = prefix + read()
                       }
                   }
                   return Local("OK").Inner().current()
               }"#,
        )
        .with_file_stem("NestedLocalMemberOwner")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn anonymous_initializer_lambda_captures_outer_receiver_and_constructor_value() {
        let inputs = [SourceInput::kotlin(
            r#"fun doSomething(block: () -> Unit) { block() }
               class Host(result: String) {
                   init {
                       val holder = object {
                           init { doSomething { completed(result) } }
                       }
                   }
                   fun completed(value: String) {}
               }"#,
        )
        .with_file_stem("AnonymousInitializerCapture")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn selected_primary_constructor_checks_singleton_default() {
        let inputs = [SourceInput::kotlin(
            r#"object Empty
               open class Base(val context: Any = Empty)
               class Derived : Base()
               fun box(): String = if (Derived().context === Empty) "OK" else "BAD""#,
        )
        .with_file_stem("PrimaryConstructorDefault")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn production_stream_keeps_explicit_backing_field_storage_distinct() {
        let inputs = [SourceInput::kotlin(
            r#"// LANGUAGE: +ExplicitBackingFields
               interface View
               class Stored(val value: Int) : View
               class Holder {
                   val item: View
                       field = Stored(42)
                   fun read(): Int = item.value
               }
               fun box(): String = if (Holder().read() == 42) "OK" else "BAD""#,
        )
        .with_file_stem("ExplicitBackingField")];
        let stems = ["ExplicitBackingField".to_string()];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &RecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(!outputs.is_empty());
    }

    #[test]
    fn bounded_local_signature_publication_uses_the_exact_classifier_root() {
        let inputs = [SourceInput::kotlin(
            r#"interface Callback { fun invoke(): String }
               open class Base(val callback: Callback)
               class Outer {
                   val ok = "OK"
                   inner class Inner : Base(object : Callback {
                       override fun invoke() = ok
                   })
               }
               fun box(): String = Outer().Inner().callback.invoke()"#,
        )
        .with_file_stem("NestedLocalSignatures")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn deep_anonymous_signature_dependencies_share_one_body_group() {
        let inputs = [SourceInput::kotlin(
            r#"interface Result { fun value(): String }
               class Host(val text: String) {
                   fun outer() = object : Result {
                       fun middle() = object : Result {
                           fun inner() = object : Result {
                               val captured = text
                               override fun value() = captured
                           }
                           override fun value() = inner().value()
                       }
                       override fun value() = middle().value()
                   }
               }
               fun box() = Host("OK").outer().value()"#,
        )
        .with_file_stem("DeepAnonymousSignatures")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn pending_free_local_callable_header_is_available_to_earlier_body_group() {
        let inputs = [SourceInput::kotlin(
            r#"class Outer {
                   private companion object { val result = "OK" }
                   class Nested {
                       fun make() = object {
                           override fun toString(): String = result
                       }
                   }
                   fun read() = Nested().make().toString()
               }
               fun box() = Outer().read()"#,
        )
        .with_file_stem("EarlyLocalCallableHeader")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let census = check_frontend_only(analysis, &mut diagnostics);
        assert!(census.is_conformant(), "{:?}", census.failures);
    }

    #[test]
    fn production_emission_requires_and_consumes_finalized_pass_one() {
        let inputs = [SourceInput::kotlin("fun box(): String = \"OK\"")];
        let stems = ["Main".to_string()];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &RecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert_eq!(outputs[0].0, "Main.out");
        assert_eq!(outputs[1], ("module.out".to_string(), b"1".to_vec()));
    }

    #[test]
    fn production_stream_checks_file_annotations_before_common_lowering() {
        let inputs = [SourceInput::kotlin(
            r#"@file:Marker("header")
               annotation class Marker(val value: String)
               fun box(): String = "OK""#,
        )
        .with_file_stem("CheckedFileAnnotation")];
        let stems = ["CheckedFileAnnotation".to_string()];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &FileAnnotationRecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert_eq!(
            outputs,
            [("file-annotations.out".to_string(), b"1".to_vec())]
        );
    }

    #[test]
    fn production_stream_attaches_checked_member_and_parameter_annotations_by_stable_identity() {
        let inputs = [SourceInput::kotlin(
            r#"annotation class Marker
               annotation class ParameterMarker
               class Host {
                   @Marker
                   fun value(@ParameterMarker input: Int): Int = input
               }"#,
        )
        .with_file_stem("CheckedMemberAnnotations")];
        let stems = ["CheckedMemberAnnotations".to_string()];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &MemberAnnotationRecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert_eq!(
            outputs,
            [("member-annotations.out".to_string(), b"3".to_vec())]
        );
    }

    #[test]
    fn production_stream_materializes_enum_entry_private_member_calls() {
        let source = r#"// WITH_STDLIB
            enum class Foo {
                FOO {
                    private fun privateBar() = "bar"
                    override fun bar(): String = privateBar()
                    override fun foo(): String = "foo"
                    override var xxx: String
                        get() = "xxx"
                        set(value: String) {}
                };
                abstract fun foo(): String
                abstract fun bar(): String
                abstract var xxx: String
            }
        "#;
        let inputs = [SourceInput::kotlin(source).with_file_stem("EnumEntryCalls")];
        let stems = ["EnumEntryCalls".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &EnumEntryCallRecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert_eq!(
            outputs,
            [("enum-entry-calls.out".to_string(), b"7".to_vec())]
        );
    }

    #[test]
    fn production_stream_lowers_anonymous_member_capture_of_extension_receiver() {
        let inputs = [SourceInput::kotlin(
            r#"interface TextView<T> { val value: T }
               fun String.view() = object : TextView<String> {
                   override val value: String = length.toString()
               }
               class Host {
                   fun String.sizeText() = this.length
                   fun String.nestedView() = object : TextView<String> {
                       override val value: String = sizeText().toString()
                   }
                   fun String.explicitView() = object : TextView<String> {
                       override val value: String = "123".sizeText().toString()
                   }
                   fun read(text: String): String = text.nestedView().value
                   fun readExplicit(text: String): String = text.explicitView().value
               }
               fun box(): String {
                   if ("OK".view().value != "2") return "BAD-1"
                   if (Host().read("OK") != "2") return "BAD-2"
                   return if (Host().readExplicit("OK") == "3") "OK" else "BAD-3"
               }"#,
        )
        .with_file_stem("CapturedExtensionReceiver")];
        let stems = ["CapturedExtensionReceiver".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &RecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(!outputs.is_empty());
    }

    #[test]
    fn production_stream_lowers_nested_local_extension_receiver_captures() {
        let inputs = [SourceInput::kotlin(
            r#"fun <T> eval(fn: () -> T) = fn()
               fun String.f(x: String): String {
                   fun String.g() = eval { this@f + this@g }
                   return x.g()
               }
               fun box() = "O".f("K")"#,
        )
        .with_file_stem("NestedLocalExtensionReceivers")];
        let stems = ["NestedLocalExtensionReceivers".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &RecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(!outputs.is_empty());
    }

    #[test]
    fn production_stream_lowers_structural_receiver_captures_through_class_storage() {
        let inputs = [
            SourceInput::kotlin(
                r#"class Host {
                   val suffix = "K"
                   fun String.local(): String {
                       class Local {
                           fun result() = this@local + this@Host.suffix
                       }
                       return Local().result()
                   }

                   fun callLocal() = "O".local()
               }

               fun localBox() = Host().callLocal()"#,
            )
            .with_file_stem("StructuralReceiverCaptures"),
            SourceInput::kotlin(
                r#"open class Base(val callback: () -> String)
               class Outer {
                   val ok = "OK"
                   inner class Inner : Base({
                       val nested = { ok }
                       nested()
                   })
               }
               fun constructorBox() = Outer().Inner().callback()"#,
            )
            .with_file_stem("ConstructorReceiverCaptures"),
        ];
        let stems = [
            "StructuralReceiverCaptures".to_string(),
            "ConstructorReceiverCaptures".to_string(),
        ];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &RecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(!outputs.is_empty());
    }

    #[test]
    fn production_stream_lowers_extension_property_receiver_in_context_function() {
        let inputs = [SourceInput::kotlin(
            r#"class C(var a: String) { fun foo(): String = a }
               val C.y
                   get() = context(x: C) fun (): String { return this@y.foo() + x.foo() }
               fun consume(x: context(C) () -> String): String = x(C("K"))
               fun box(): String = consume(C::y.get(C("O")))"#,
        )
        .with_file_stem("ExtensionPropertyContextFunction")];
        let stems = ["ExtensionPropertyContextFunction".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &RecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(!outputs.is_empty());
    }

    #[test]
    fn production_emission_never_lowers_without_finalized_signatures() {
        let inputs = [SourceInput::kotlin("fun a() = b()\nfun b() = a()")];
        let stems = ["Cycle".to_string()];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &RecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(outputs.is_empty());
        assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| (diagnostic.file, diagnostic.span, diagnostic.msg.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    0,
                    crate::diag::Span::new(10, 13),
                    "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.",
                ),
                (
                    0,
                    crate::diag::Span::new(24, 27),
                    "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.",
                ),
            ]
        );
    }

    #[test]
    fn production_stream_preserves_cross_file_calls_by_stable_module_identity() {
        let inputs = [
            SourceInput::kotlin("fun answer(): Int = helper()").with_file_stem("Caller"),
            SourceInput::kotlin("fun helper(): Int = 42").with_file_stem("Helper"),
        ];
        let stems = ["Caller".to_string(), "Helper".to_string()];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &ModuleCallRecordingBackend,
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert_eq!(outputs, [("module-calls.out".to_string(), b"1".to_vec())]);
    }

    #[test]
    fn jvm_production_stream_realizes_cross_file_module_calls_after_fir() {
        let inputs = [
            SourceInput::kotlin("fun answer(): Int = helper()").with_file_stem("Caller"),
            SourceInput::kotlin("fun helper(): Int = 42").with_file_stem("Helper"),
        ];
        let stems = ["Caller".to_string(), "Helper".to_string()];
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs.iter().any(|(path, _)| path == "CallerKt.class"));
        assert!(outputs.iter().any(|(path, _)| path == "HelperKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_external_calls_by_provider_identity() {
        let inputs = [SourceInput::kotlin(
            r#"fun box(): String {
                val member = "abc".substring(1)
                val extension = "x".repeat(2)
                val length = "abc".length
                val indices = "abc".indices
                val builder = StringBuilder("abc")
                println(member)
                if (length != 3) return "BAD"
                if (indices.first != 0) return "BAD"
                if (builder.toString() != "abc") return "BAD"
                return member + extension
            }"#,
        )
        .with_file_stem("ExternalCalls")];
        let stems = ["ExternalCalls".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "ExternalCallsKt.class"));
    }

    #[test]
    fn jvm_production_stream_materializes_source_property_storage() {
        let inputs = [SourceInput::kotlin(
            r#"val top: Int = 2

            class Box {
                val value: Int = 1
            }

            fun box(): String = if (top + Box().value == 3) "OK" else "BAD""#,
        )
        .with_file_stem("SourceProperties")];
        let stems = ["SourceProperties".to_string()];
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "SourcePropertiesKt.class"));
        assert!(outputs.iter().any(|(path, _)| path == "Box.class"));
    }

    #[test]
    fn pass_one_publishes_cross_file_primary_constructor_identity() {
        let inputs = [
            SourceInput::kotlin("fun use(): Holder = Holder(7)").with_file_stem("Use"),
            SourceInput::kotlin("class Holder(val value: Int)").with_file_stem("Holder"),
        ];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let index = analysis
            .streamed
            .as_ref()
            .expect("Pass 1 module")
            .module
            .index();
        let constructor = index
            .constructor_declaration(crate::types::type_name("Holder"), true, &[Ty::Int])
            .expect("stable primary constructor");
        assert!(index.callable_for_declaration(constructor).is_some());
    }

    #[test]
    fn jvm_production_stream_realizes_cross_file_properties_and_constructors() {
        let inputs = [
            SourceInput::kotlin(
                "fun box(): String = if (answer == 42 && Holder(7).value == 7) \"OK\" else \"BAD\"",
            )
            .with_file_stem("Use"),
            SourceInput::kotlin("val answer: Int = 42\nclass Holder(val value: Int)")
                .with_file_stem("Declarations"),
        ];
        let stems = ["Use".to_string(), "Declarations".to_string()];
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs.iter().any(|(path, _)| path == "UseKt.class"));
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "DeclarationsKt.class"));
        assert!(outputs.iter().any(|(path, _)| path == "Holder.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_checked_sam_lambda() {
        let inputs = [SourceInput::kotlin(
            r#"fun interface Action {
                fun run(value: Int): String
            }

            fun consume(action: Action): String = action.run(42)
            fun box(): String = consume { value -> if (value == 42) "OK" else "BAD" }"#,
        )
        .with_file_stem("SamLambda")];
        let stems = ["SamLambda".to_string()];
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs.iter().any(|(path, _)| path == "SamLambdaKt.class"));
        assert!(outputs.iter().any(|(path, _)| path == "Action.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_source_object_receiver() {
        let inputs = [SourceInput::kotlin(
            r#"object Values {
                val answer: Int = 42
            }

            fun box(): String = if (Values.answer == 42) "OK" else "BAD""#,
        )
        .with_file_stem("ObjectReceiver")];
        let stems = ["ObjectReceiver".to_string()];
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "ObjectReceiverKt.class"));
        assert!(outputs.iter().any(|(path, _)| path == "Values.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_cross_file_object_receiver() {
        let inputs = [
            SourceInput::kotlin("fun box(): String = if (Values.answer == 42) \"OK\" else \"BAD\"")
                .with_file_stem("UseObject"),
            SourceInput::kotlin("object Values { val answer: Int = 42 }")
                .with_file_stem("ObjectDeclaration"),
        ];
        let stems = ["UseObject".to_string(), "ObjectDeclaration".to_string()];
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs.iter().any(|(path, _)| path == "UseObjectKt.class"));
        assert!(outputs.iter().any(|(path, _)| path == "Values.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_checked_class_literals() {
        let inputs = [SourceInput::kotlin(
            r#"fun literals(value: String): String {
                val unbound = String::class
                val bound = value::class
                return if (unbound == bound) "OK" else "BAD"
            }
            fun inferredUnbound() = String::class
            fun inferredBound(value: String) = value::class
            fun inferredPrimitive() = Int::class
            fun explicitPrimitive(): kotlin.reflect.KClass<Int> = Int::class
            val topLevelValue: String = "value"
            fun inferredTopLevelBound() = topLevelValue::class
            class LiteralHolder(val value: String)
            class SelfLiteral { fun inferredSelf() = this::class }
            val topLevelHolder: LiteralHolder = LiteralHolder("value")
            fun inferredQualifiedBound() = topLevelHolder.value::class"#,
        )
        .with_file_stem("ClassLiterals")];
        let stems = ["ClassLiterals".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        let streamed = analysis
            .streamed
            .as_ref()
            .expect("class literal signatures must finalize before Pass 2");
        let signature = |name: &str| {
            (0..streamed.module.index().declaration_count())
                .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
                .find_map(|declaration| {
                    (streamed.module.index().declaration_name(declaration) == Some(name))
                        .then(|| streamed.module.index().signature(declaration))
                        .flatten()
                })
                .expect("named class-literal declaration")
                .result
                .get()
        };
        assert_eq!(
            signature("inferredPrimitive"),
            signature("explicitPrimitive")
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "ClassLiteralsKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_source_iterator_loop() {
        let inputs = [SourceInput::kotlin(
            r#"class WordsIterator {
                operator fun hasNext(): Boolean = false
                operator fun next(): String = "word"
            }
            class Words { operator fun iterator(): WordsIterator = WordsIterator() }
            fun consume(words: Words) { for (word in words) { word } }"#,
        )
        .with_file_stem("IteratorLoop")];
        let stems = ["IteratorLoop".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "IteratorLoopKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_dependency_iterator_loop() {
        let inputs = [SourceInput::kotlin(
            r#"fun consume(values: List<Int>): Int {
                var total = 0
                for (value in values) total += value
                return total
            }"#,
        )
        .with_file_stem("DependencyIterator")];
        let stems = ["DependencyIterator".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "DependencyIteratorKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_source_callable_references() {
        let inputs = [SourceInput::kotlin(
            r#"fun increment(value: Int): Int = value + 1
            fun withDefault(value: Int, amount: Int = 1): Int = value + amount
            fun sum(vararg values: Int): Int = values[0] + values[1]
            fun <T> identity(value: T): T = value
            fun String.append(value: String): String = this + value
            class Counter { fun increment(value: Int): Int = value + 1 }
            fun references(counter: Counter): Int {
                val topLevel = ::increment
                val bound = counter::increment
                val unbound = Counter::increment
                val defaulted: (Int) -> Int = ::withDefault
                val packed: (Int, Int) -> Int = ::sum
                val generic: (String) -> String = ::identity
                val boundExtension = "a"::append
                val unboundExtension = String::append
                return topLevel(1) + bound(1) + unbound(counter, 1) + defaulted(1) + packed(1, 2) + generic("x").length + boundExtension("b").length + unboundExtension("a", "b").length
            }"#,
        )
        .with_file_stem("SourceReferences")];
        let stems = ["SourceReferences".to_string()];
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "SourceReferencesKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_cross_file_callable_reference() {
        let inputs = [
            SourceInput::kotlin("fun referenced(value: Int): Int = value + 1")
                .with_file_stem("ReferenceTarget"),
            SourceInput::kotlin(
                "fun invokeReference(): Int { val reference = ::referenced; return reference(41) }",
            )
            .with_file_stem("ReferenceCaller"),
        ];
        let stems = ["ReferenceTarget".to_string(), "ReferenceCaller".to_string()];
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "ReferenceTargetKt.class"));
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "ReferenceCallerKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_capturing_local_function_reference() {
        let inputs = [SourceInput::kotlin(
            r#"fun apply(value: Int, operation: (Int) -> Int): Int = operation(value)

            fun box(): String {
                val base = 40
                fun add(value: Int): Int = base + value
                return if (apply(2, ::add) == 42) "OK" else "BAD"
            }"#,
        )
        .with_file_stem("LocalReference")];
        let stems = ["LocalReference".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "LocalReferenceKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_local_defaults_and_adapted_reference() {
        let inputs = [SourceInput::kotlin(
            r#"enum class FirChoice { OK }

            fun suspendReference(): suspend (Int) -> Int {
                fun increment(value: Int): Int = value + 1
                return ::increment
            }

            fun box(): String {
                suspendReference()
                val prefix = ""
                fun join(value: String = prefix, suffix: String = "K"): String = value + suffix
                fun sum(vararg values: Int): Int = values[0] + values[1]
                fun String.tag(suffix: String): String = this + suffix
                val reference: (String) -> String = ::join
                val sumReference: (Int, Int) -> Int = ::sum
                val tagReference: (String) -> String = "O"::tag
                val dependencyReference: (String) -> String = String::uppercase
                val boundDependencyReference: () -> String = "ok"::uppercase
                val topLevelDependencyReference: () -> List<String> = ::emptyList
                val adaptedDependencyReference: (String, Int) -> String = String::padEnd
                val boundAdaptedDependencyReference: (Int) -> String = "x"::padEnd
                val varargDependencyReference: (String, String) -> List<String> = ::listOf
                val suspendDependencyReference: suspend (String) -> String = String::uppercase
                val propertyDependencyReference: (String) -> Int = String::length
                val boundPropertyDependencyReference: () -> Int = "OK"::length
                val reflectivePropertyDependencyReference = String::length
                val boundReflectivePropertyDependencyReference = "OK"::length
                val classifierPropertyReference = FirChoice::entries
                return if (sum(1, 2) == 3 && sumReference(1, 2) == 3 &&
                    "O".tag("K") == "OK" && tagReference("K") == "OK" &&
                    dependencyReference("ok") == "OK" && boundDependencyReference() == "OK" &&
                    topLevelDependencyReference().isEmpty() &&
                    adaptedDependencyReference("x", 2) == "x " &&
                    boundAdaptedDependencyReference(2) == "x " &&
                    varargDependencyReference("O", "K").size == 2 &&
                    propertyDependencyReference("OK") == 2 &&
                    boundPropertyDependencyReference() == 2 &&
                    reflectivePropertyDependencyReference.get("OK") == 2 &&
                    boundReflectivePropertyDependencyReference.get() == 2 &&
                    classifierPropertyReference.get()[0].name == "OK" &&
                    suspendDependencyReference.toString().isNotEmpty()) {
                    join(suffix = "K") + reference("")
                } else "BAD"
            }"#,
        )
        .with_file_stem("LocalDefaults")];
        let stems = ["LocalDefaults".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "LocalDefaultsKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_local_delegated_property() {
        let inputs = [SourceInput::kotlin(
            r#"class Delegate {
                operator fun getValue(owner: Any?, property: Any?): String = "OK"
                operator fun setValue(owner: Any?, property: Any?, value: String) {}
            }

            fun box(): String {
                var value by Delegate()
                value = "OK"
                return value
            }"#,
        )
        .with_file_stem("LocalDelegate")];
        let stems = ["LocalDelegate".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "LocalDelegateKt.class"));
    }

    #[test]
    fn jvm_production_stream_realizes_non_capturing_local_class() {
        let inputs = [SourceInput::kotlin(
            r#"fun box(): String {
                class Local(val value: String) {
                    fun read(): String = value
                }
                return Local("OK").read()
            }"#,
        )
        .with_file_stem("LocalClassifier")];
        let stems = ["LocalClassifier".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "LocalClassifierKt.class"));
        assert!(outputs.iter().any(|(path, _)| path.contains("Local")));
    }

    #[test]
    fn jvm_production_stream_realizes_capturing_local_class() {
        let inputs = [SourceInput::kotlin(
            r#"class Outer(val prefix: String) {
                fun result(): String {
                    val suffix = "K"
                    class Local(val own: String) {
                        fun read(): String {
                            fun local(): String {
                                class Nested {
                                    fun value(): String = suffix
                                }
                                return Nested().value()
                            }
                            return prefix + local() + own
                        }
                    }
                    return Local("").read()
                }
            }

            fun nested(): String {
                val make = {
                    val prefix = "O"
                    class Nested {
                        fun read(): String = prefix + "K"
                    }
                    Nested().read()
                }
                return make()
            }

            fun box(): String = Outer("O").result() + nested()"#,
        )
        .with_file_stem("CapturingLocalClassifier")];
        let stems = ["CapturingLocalClassifier".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "CapturingLocalClassifierKt.class"));
        assert!(outputs.iter().any(|(path, _)| path.contains("Local")));
    }

    #[test]
    fn jvm_production_stream_publishes_inherited_local_class_members() {
        let inputs = [SourceInput::kotlin(
            r#"fun hierarchy(captured: String): String {
                open class Local {
                    fun value() = captured
                }
                open class Derived : Local() {
                    fun inherited() = value()
                }
                return Derived().inherited()
            }

            fun box(): String = hierarchy("OK")"#,
        )
        .with_file_stem("LocalHierarchy")];
        let stems = ["LocalHierarchy".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "LocalHierarchyKt.class"));
        assert!(outputs.iter().any(|(path, _)| path.contains("Derived")));
    }

    #[test]
    fn jvm_production_stream_resolves_self_instantiation_from_inner_local_class() {
        let inputs = [SourceInput::kotlin(
            r#"fun box(): String {
                val capturedInConstructor = 1
                val capturedInBody = 10
                class C(var x: Int) {
                    var y = 0

                    inner class D {
                        fun copyOuter(): C {
                            val result = C(x)
                            result.y += capturedInBody
                            return result
                        }
                    }

                    init {
                        y += x + capturedInConstructor
                    }
                }

                val result = C(100).D().copyOuter()
                return if (result.x == 100 && result.y == 111) "OK" else "fail"
            }"#,
        )
        .with_file_stem("LocalSelfInstantiation")];
        let stems = ["LocalSelfInstantiation".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "LocalSelfInstantiationKt.class"));
        assert!(outputs.iter().any(|(path, _)| path.contains("$C$D")));
    }

    #[test]
    fn jvm_production_stream_captures_anonymous_receiver_in_accessor_lambda() {
        let inputs = [SourceInput::kotlin(
            r#"fun build(): Int {
                var count = 0
                val holder = object {
                    val action: () -> Unit get() = { count++ }
                }
                holder.action()
                return count
            }

            fun box(): String = if (build() == 1) "OK" else "fail""#,
        )
        .with_file_stem("AnonymousAccessorCapture")];
        let stems = ["AnonymousAccessorCapture".to_string()];
        let mut paths = Vec::new();
        if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
            paths.push(stdlib);
        }
        if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
            paths.push(jdk);
        }
        let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
                classpath.clone(),
            )),
            &LangFeatures::new(),
            |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );

        let outputs = emit_analyzed(
            analysis,
            &stems,
            &crate::jvm::JvmBackend::new(classpath),
            "main",
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(outputs
            .iter()
            .any(|(path, _)| path == "AnonymousAccessorCaptureKt.class"));
        assert!(outputs.iter().any(|(path, _)| path.contains("build$1")));
    }

    #[test]
    fn compiler_orchestrates_frontend_then_backend() {
        let mut diags = DiagSink::new();
        let files = vec![parse_source_with_detected_features(
            "fun box(): String = \"OK\"",
            &mut diags,
        )];
        let stems = vec!["Main".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);
        let outputs = compile(
            &files,
            &stems,
            &mut syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].0, "Main.out");
        assert_eq!(outputs[1], ("module.out".to_string(), b"1".to_vec()));
    }

    #[test]
    fn compiler_does_not_lower_after_frontend_error() {
        let mut diags = DiagSink::new();
        let files = vec![parse_source_with_detected_features(
            "fun box(): Int = \"no\"",
            &mut diags,
        )];
        let stems = vec!["Main".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);
        let outputs = compile(
            &files,
            &stems,
            &mut syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert!(diags.has_errors());
        assert!(outputs.is_empty());
    }

    #[test]
    fn oversized_conflicting_overload_signature_still_blocks_lowering() {
        let parameter = "value".repeat(14 * 1024);
        let source = format!("fun crowded({parameter}: Int): Int = 0");
        let mut diags = DiagSink::new();
        let files = vec![
            parse_source_with_detected_features(&source, &mut diags),
            parse_source_with_detected_features(&source, &mut diags),
        ];
        let stems = vec!["First".to_string(), "Second".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);
        let outputs = compile(
            &files,
            &stems,
            &mut syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert!(diags.has_errors());
        assert!(diags
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.starts_with("conflicting overloads:")));
        assert!(outputs.is_empty());
    }

    #[test]
    fn same_file_private_and_public_signature_conflict_blocks_lowering() {
        let source = "fun crowded(value: Int): Int = value\n\
                      private fun crowded(value: Int): Int = value";
        let mut diags = DiagSink::new();
        let files = vec![parse_source_with_detected_features(source, &mut diags)];
        let stems = vec!["Main".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);
        let outputs = compile(
            &files,
            &stems,
            &mut syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert!(diags.has_errors());
        assert_eq!(
            diags
                .diags
                .iter()
                .filter(|diagnostic| diagnostic.msg.starts_with("conflicting overloads:"))
                .count(),
            2
        );
        assert!(outputs.is_empty());
    }

    #[test]
    fn cross_file_private_context_function_cannot_reach_lowering() {
        let mut diags = DiagSink::new();
        let files = vec![
            parse_source_with_detected_features(
                "fun <T, R> with(receiver: T, block: T.() -> R): R = receiver.block()\n\
                 class Scope\n\
                 fun use(scope: Scope): Int = with(scope) { hidden(1) }",
                &mut diags,
            ),
            parse_source_with_detected_features(
                "private context(scope: Scope) fun hidden(value: Int): Int = value",
                &mut diags,
            ),
        ];
        let stems = vec!["Caller".to_string(), "Hidden".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);
        let outputs = compile(
            &files,
            &stems,
            &mut syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert_eq!(
            diags
                .diags
                .iter()
                .filter(|diagnostic| diagnostic.file == 0)
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            ["cannot access 'hidden': it is private in its file"]
        );
        assert!(outputs.is_empty());
    }

    #[test]
    fn compiler_does_not_emit_kotlin_scripts() {
        let source = "val value = 1";
        let mut diags = DiagSink::new();
        let tokens = lex(source, &mut diags);
        let files = vec![parse_script_with_features(
            source,
            &tokens,
            &mut diags,
            &LangFeatures::new(),
        )];
        let stems = vec!["Script".to_string()];
        let mut syms = collect_signatures(&files, &mut diags);
        let outputs = compile(
            &files,
            &stems,
            &mut syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert!(outputs.is_empty());
        assert!(diags
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.contains("cannot be emitted")));
    }

    #[test]
    fn checked_emission_rejects_misaligned_source_metadata() {
        let mut diags = DiagSink::new();
        let files = vec![parse_source_with_detected_features(
            "fun box(): String = \"OK\"",
            &mut diags,
        )];
        let syms = collect_signatures(&files, &mut diags);
        let outputs = emit_checked(
            &files,
            &[],
            &[],
            &syms,
            &RecordingBackend,
            "main",
            &mut diags,
        );

        assert!(outputs.is_empty());
        assert!(diags.has_errors());
    }
}
