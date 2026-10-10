//! Compiler orchestration.

mod backend_handoff;
mod declaration_metadata;
mod diagnostic_recovery;
mod local_visibility;
mod metadata_handoff;
#[cfg(test)]
mod streaming_tests;

use crate::ast::File;
pub use crate::backend::{Artifact, Backend, CheckedIrFile};
use crate::diag::{DiagSink, Span};
use crate::frontend::StreamingSourceSetAnalysis;
use diagnostic_recovery::recover_pass_two_diagnostics;

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
    let _supertype_projection = crate::resolve::SupertypeProjectionCache::enter();
    let StreamingSourceSetAnalysis {
        target,
        symbols,
        reparse_sources,
        streamed,
    } = analysis.into();
    if target != backend.compilation_target() {
        // A checked program carries its target's source rules (which `value class` is inline,
        // for one), so another target's backend cannot consume it.
        diags.error(
            Span::new(0, 0),
            format!(
                "internal error: the source set was analyzed for {target:?} but the backend emits for {:?}",
                backend.compilation_target()
            ),
        );
        return Vec::new();
    }
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
        ir.recursive_type_of = source.recursive_type_of();
        ir.source_debug = crate::ir::SourceDebug::from_map(&source_map, source_id);
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
        if let Err(error) = sink.accept_inline_bodies(&index, &mut inline_bodies) {
            diags.error(
                Span::new(0, 0),
                format!("internal error: cannot consume inline FIR: {error:?}"),
            );
            continue;
        }
        if let Err(error) = sink.accept_default_arguments(&index, &mut default_arguments) {
            crate::trace_compiler!("fir", "default FIR lowering failed: {error:?}");
            diags.error(
                Span::new(0, 0),
                format!("internal error: default FIR lowering failed: {error:?}"),
            );
            continue;
        }
        let mut source_failed = false;
        let mut source_rejected = false;
        let source_diagnostics = diags.diags.len();
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
            metadata_handoff::accept_active_debug_metadata(
                &index,
                &active_file,
                &active,
                sink.ir_mut(),
            );
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
            // Property storage/accessor realizations can be created while consuming the group, so
            // refresh their line-only metadata before this bounded parser unit is dropped.
            metadata_handoff::accept_active_debug_metadata(
                &index,
                &active_file,
                &active,
                sink.ir_mut(),
            );
            // `active_file`, its AST-keyed semantic tables, and checked FIR temporaries all
            // drop when this callback returns, before the parser resumes with the next unit.
        });
        // kotlinc lists a file's diagnostics by position, whichever member its checker visited first.
        diags.sort_source_order_from(source_diagnostics);
        if !source_failed && !declaration_cursor.is_finished() {
            diags.error(
                Span::new(0, 0),
                "internal error: declaration header stream was not consumed by source reparsing",
            );
            source_failed = true;
        }
        if source_failed || source_rejected {
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
        let Some(file) = backend_handoff::checked_ir_file(
            ir,
            source_id,
            &backend_module_facts,
            &symbols,
            module_name,
            stems,
            diags,
        ) else {
            continue;
        };
        outputs.extend(backend.lower_ir_file(file, &mut state, diags));
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
    /// Lowers for whatever target the analysis was checked for: common IR is target-neutral.
    struct DiscardCommonIr(crate::compilation_target::CompilationTarget);

    impl Backend for DiscardCommonIr {
        type State = ();

        fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
            self.0
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

    let analysis = analysis.into();
    let backend = DiscardCommonIr(analysis.target);
    let outputs = emit_analyzed(analysis, stems, &backend, module_name, diags);
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
    /// Local and anonymous classifiers declared inside this group's bodies. Checking those bodies
    /// checks them, so their declaration metadata is handed off with this group's result.
    local_classifiers: Vec<crate::fir::DeclarationId>,
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
                        local_classifiers: Vec::new(),
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
    let mut groups = body_check_groups(rooted);
    // Declaration annotations and other checked header metadata are consumed during this same
    // bounded Pass-2 reparse. A classifier with no executable body (most notably an interface or
    // annotation class) still needs one semantic group; tying metadata handoff to body presence
    // silently dropped it. This group selects no body and retains no syntax/FIR after the callback.
    for declaration in active.stable_declarations().filter(|declaration| {
        index
            .declaration_header(*declaration)
            .is_some_and(|header| header.kind == crate::fir::DeclarationKind::Classifier)
    }) {
        if groups.iter().any(|group| group.root == declaration) {
            continue;
        }
        // A local classifier is checked with the body that declares it, so its metadata belongs
        // to the innermost group whose root encloses it. A group of its own would check nothing.
        let local = index
            .declaration_header(declaration)
            .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS));
        let enclosing = local
            .then(|| active.span(file, declaration))
            .flatten()
            .and_then(|classifier_span| {
                groups
                    .iter()
                    .enumerate()
                    .filter(|(_, group)| !group.work.is_empty())
                    .filter_map(|(position, group)| {
                        active.span(file, group.root).map(|span| (position, span))
                    })
                    .filter(|(_, span)| {
                        span.lo <= classifier_span.lo && classifier_span.hi <= span.hi
                    })
                    .min_by_key(|(_, span)| span.hi - span.lo)
                    .map(|(position, _)| position)
            });
        if let Some(position) = enclosing {
            groups[position].local_classifiers.push(declaration);
            continue;
        }
        groups.push(BodyCheckGroup {
            root: declaration,
            bodies: std::collections::HashSet::new(),
            work: Vec::new(),
            local_classifiers: Vec::new(),
        });
    }
    groups.sort_by_key(|group| {
        active
            .span(file, group.root)
            .map_or((u32::MAX, u32::MAX, group.root), |span| {
                (span.lo, span.hi, group.root)
            })
    });
    groups
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
    // Most local classifiers retain public/default visibility, so their first checked body is
    // already final. Repeat the body check only when publishing an override plan actually made a
    // modifier-less member non-public; that later access check must consume the corrected header.
    // Re-entering every body that merely contains a local classifier makes already-published local
    // inheritance edges look like a second declaration and produces spurious `overrides nothing`.
    let inherited_local_visibility =
        local_visibility::group_published_non_public_member(active_file, active, &info, index);
    if !inherited_local_visibility {
        return Some(info);
    }

    // A body-local classifier does not have a stable checked signature until the first traversal
    // has entered its lexical statement and the bridge above has published that signature. The
    // inherited override plan is part of that signature: member access later in the same enclosing
    // body must therefore be checked against the published plan, not the provisional parser
    // declaration. Repeat the ordinary checker after publication instead of duplicating access or
    // override semantics at this orchestration boundary. The first traversal is discovery-only for
    // this group, so replace its diagnostics with the authoritative traversal's exact sequence.
    diags.diags.truncate(diagnostics_start);
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
        diags,
    );
    diags.collapse_duplicates_from(diagnostics_start);
    if diags.diags[diagnostics_start..]
        .iter()
        .any(|diagnostic| diagnostic.severity == crate::diag::Severity::Error)
    {
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
            crate::fir::CheckedBodyDriverFailure::Check(crate::fir::BodyCheckFailure {
                span,
                kind: crate::fir::BodyCheckFailureKind::RecursiveTypeOf(parameter),
            }) => {
                diags.error(
                    span.unwrap_or_else(|| Span::new(0, 0)),
                    format!(
                        "non-reified type parameters with recursive bounds are not supported yet: {parameter}"
                    ),
                );
            }
            _ => diags.error(
                Span::new(0, 0),
                format!("internal error: checked FIR construction failed: {error:?}"),
            ),
        }
        return false;
    }
    for root in std::iter::once(group.root).chain(group.local_classifiers.iter().copied()) {
        metadata_handoff::attach_checked_declaration_metadata(
            active_file,
            active,
            &info,
            source_id,
            root,
            index,
            sink.ir_mut(),
        );
    }
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
        target: _,
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
        let mut source_rejected = false;
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
                    if internal {
                        source_failed = true;
                        return;
                    }
                    // An ordinary semantic rejection invalidates only this bounded body group.
                    // Keep binding and checking later declaration units so frontend diagnostics
                    // are complete; the successful emission path follows the same rule.
                    source_rejected = true;
                    continue;
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
        if !source_failed && !source_rejected {
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

#[cfg(test)]
mod tests;
