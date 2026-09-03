//! Pass-1 checking and retention of signature-owned executable fragments.
//!
//! The production path checks one source's selected inline declarations and parameter defaults,
//! moves their checked FIR into their dedicated stores, and drops that source's `TypeInfo` before
//! advancing. No parser coordinate or default provider work survives into Pass 2.

use super::{File, FrontendSymbols, FrontendTypeInfo, StreamedPassState};

#[derive(Default)]
struct UnexpectedOrdinaryInlineBody(usize);

impl crate::fir::CheckedBodySink for UnexpectedOrdinaryInlineBody {
    fn accept_finalized(&mut self, _owner: crate::fir::BodyOwnerId, _body: crate::fir::FirBody) {
        self.0 += 1;
    }
}

#[derive(Default)]
struct InlineNestedBodyCollector {
    roots: std::collections::HashMap<crate::fir::DeclarationId, crate::fir::CallableId>,
    bodies: Vec<(crate::fir::CallableId, crate::fir::FirBody)>,
    missing_owner: Option<crate::fir::DeclarationId>,
}

impl crate::fir::CheckedBodySink for InlineNestedBodyCollector {
    fn accept_finalized(&mut self, owner: crate::fir::BodyOwnerId, body: crate::fir::FirBody) {
        let declaration = crate::fir::DeclarationId::from_raw(owner.raw());
        let Some(root) = self.roots.get(&declaration).copied() else {
            self.missing_owner = Some(declaration);
            return;
        };
        self.bodies.push((root, body));
    }
}

fn inline_payload_roots(
    selection: &crate::fir::BodyCheckSelection,
    index: &crate::fir::ResolvedModuleIndex,
) -> Option<std::collections::HashMap<crate::fir::DeclarationId, crate::fir::CallableId>> {
    selection
        .payload_roots
        .iter()
        .map(|(&nested, &root)| {
            index
                .callable_for_declaration(root)
                .map(|callable| (nested, callable.id))
        })
        .collect()
}

fn attach_inline_nested_bodies(
    inline_bodies: &mut crate::fir::InlineBodyStore,
    collector: InlineNestedBodyCollector,
) -> bool {
    if collector.missing_owner.is_some() {
        return false;
    }
    for (root, body) in collector.bodies {
        inline_bodies.attach_nested_declaration_body(root, body);
    }
    true
}

struct DefaultArgumentSink<'a> {
    index: &'a crate::fir::ResolvedModuleIndex,
    store: &'a mut crate::fir::DefaultArgumentStore,
    missing_owner: bool,
}

impl crate::fir::CheckedBodySink for DefaultArgumentSink<'_> {
    fn accept_finalized(&mut self, owner: crate::fir::BodyOwnerId, body: crate::fir::FirBody) {
        let declaration = crate::fir::DeclarationId::from_raw(owner.raw());
        let Some(callable) = self.index.callable_for_declaration(declaration) else {
            self.missing_owner = true;
            return;
        };
        self.store.insert(callable, body);
    }
}

struct DefaultCheckSelection {
    roots: Vec<std::collections::HashSet<crate::fir::DeclarationId>>,
    bodies: Vec<std::collections::HashSet<crate::fir::DeclarationId>>,
}

fn default_check_selection(
    providers: &[crate::fir::DefaultArgumentProvider],
    index: &crate::fir::ResolvedModuleIndex,
    headers: &crate::fir::StreamedHeaderModule,
    source_count: usize,
) -> Option<DefaultCheckSelection> {
    let mut roots = vec![std::collections::HashSet::new(); source_count];
    let mut bodies = vec![std::collections::HashSet::new(); source_count];
    for work in providers {
        let Some(provider) = index.declaration_anchor(work.provider) else {
            crate::trace_compiler!(
                "fir",
                "Pass 1 default provider has no finalized anchor: {:?}",
                work.provider,
            );
            return None;
        };
        let root_id = headers.lexical_root_for_default(index, work.provider);
        let Some(root) = index.declaration_anchor(root_id) else {
            crate::trace_compiler!(
                "fir",
                "Pass 1 default lexical root has no finalized anchor provider={:?} root={root_id:?}",
                work.provider,
            );
            return None;
        };
        if root.source != provider.source {
            return None;
        }
        let Some(selected_bodies) = bodies.get_mut(provider.source.raw() as usize) else {
            return None;
        };
        // The provider selects the live syntax; the target selects the surviving semantic
        // declaration after expect/actual actualization. `bind_defaults` aliases both identities
        // to that one parser declaration for this bounded check, so selection must admit both.
        selected_bodies.insert(work.provider);
        selected_bodies.insert(work.target);
        let Some(selected_roots) = roots.get_mut(root.source.raw() as usize) else {
            return None;
        };
        selected_roots.insert(root_id);
    }
    Some(DefaultCheckSelection { roots, bodies })
}

/// Check every signature-owned default immediately after signature finalization, while the
/// initial Pass-1 parser and compact header environment are still live. Only the detached checked
/// FIR returned from this function survives consumption of `headers`; provider/root selection is
/// transient stack state.
#[allow(clippy::too_many_arguments)]
pub(super) fn defaults(
    headers: &mut crate::fir::StreamedHeaderModule,
    index: &mut crate::fir::ResolvedModuleIndex,
    mut providers: Vec<crate::fir::DefaultArgumentProvider>,
    files: &[File],
    skip: &[bool],
    checked_count: usize,
    symbols: &mut FrontendSymbols,
    diags: &mut crate::diag::DiagSink,
) -> Option<crate::fir::DefaultArgumentStore> {
    providers.sort_by_key(|provider| (provider.target, provider.provider, provider.relation));
    providers.dedup();
    let selection = default_check_selection(&providers, index, headers, files.len())?;
    let mut store = crate::fir::DefaultArgumentStore::default();
    let mut sessions = (0..files.len())
        .map(|_| crate::fir::BodyCheckSession::default())
        .collect::<Vec<_>>();
    for raw_source in 0..files.len() {
        if selection.bodies[raw_source].is_empty() {
            continue;
        }
        if raw_source >= checked_count || skip.get(raw_source).copied().unwrap_or(false) {
            crate::trace_compiler!(
                "fir",
                "Pass 1 signature-default source {raw_source} is not checkable",
            );
            return None;
        }
        diags.set_file(raw_source as u32);
        if !check_default_source(
            &files[raw_source],
            raw_source,
            &selection,
            &mut providers,
            symbols,
            index,
            &mut headers.sources,
            &mut store,
            &mut sessions[raw_source],
            diags,
        ) {
            return None;
        }
    }
    assert!(
        providers.is_empty(),
        "every signature default must be checked and stored during Pass 1"
    );
    Some(store)
}

#[allow(clippy::too_many_arguments)]
fn check_default_source(
    file: &File,
    raw_source: usize,
    selection: &DefaultCheckSelection,
    providers: &mut Vec<crate::fir::DefaultArgumentProvider>,
    symbols: &mut FrontendSymbols,
    index: &mut crate::fir::ResolvedModuleIndex,
    sources: &mut crate::fir::SourceMap,
    store: &mut crate::fir::DefaultArgumentStore,
    session: &mut crate::fir::BodyCheckSession,
    diags: &mut crate::diag::DiagSink,
) -> bool {
    let source = crate::fir::SourceFileId::from_raw(raw_source as u32);
    let mut items = Vec::new();
    providers.retain(|provider| {
        let selected = index
            .declaration_anchor(provider.provider)
            .is_some_and(|anchor| anchor.source == source);
        if selected {
            items.push(*provider);
        }
        !selected
    });
    crate::trace_compiler!(
        "fir",
        "Pass 1 signature-default selection source={source:?} roots={:?} providers={:?}",
        selection.roots[raw_source],
        selection.bodies[raw_source],
    );
    let active = match crate::fir::ActiveSourceDeclarations::bind_defaults(
        file,
        source,
        index,
        &selection.roots[raw_source],
        &selection.bodies[raw_source],
        &items,
    ) {
        Ok(active) => active,
        Err(error) => {
            crate::trace_compiler!(
                "fir",
                "Pass 1 signature-default source binding failed source={source:?}: {error:?}",
            );
            return false;
        }
    };
    let mut diagnostic_ranges = Vec::new();
    for provider in items.iter().map(|item| item.provider) {
        if let Some(function) = active.function(file, provider) {
            diagnostic_ranges.extend(
                function
                    .params
                    .iter()
                    .filter_map(|parameter| parameter.default)
                    .filter_map(|expression| file.expr_span(expression)),
            );
            continue;
        }
        if let Some((_, class, secondary)) = active.constructor(file, provider) {
            diagnostic_ranges.extend(
                class
                    .context_params
                    .iter()
                    .filter_map(|parameter| parameter.default)
                    .filter_map(|expression| file.expr_span(expression)),
            );
            match secondary {
                Some(constructor) => diagnostic_ranges.extend(
                    constructor
                        .params
                        .iter()
                        .filter_map(|parameter| parameter.default)
                        .filter_map(|expression| file.expr_span(expression)),
                ),
                None => diagnostic_ranges.extend(
                    class
                        .props
                        .iter()
                        .filter_map(|parameter| parameter.default)
                        .filter_map(|expression| file.expr_span(expression)),
                ),
            }
        }
    }
    diagnostic_ranges.sort_by_key(|range| (range.lo, range.hi));
    diagnostic_ranges.dedup();
    let info = diags.with_authoritative_ranges(&diagnostic_ranges, |diags| {
        crate::resolve::check_signature_default_declarations_at_with_index(
            file,
            raw_source as u32,
            &active,
            &selection.roots[raw_source],
            &selection.bodies[raw_source],
            symbols,
            index,
            diags,
        )
    });
    let provider_declarations = items.iter().map(|item| item.provider).collect::<Vec<_>>();
    if let Err(declarations) = crate::resolve::publish_checked_default_local_signatures(
        file,
        &active,
        source,
        symbols,
        &info,
        index,
        &provider_declarations,
    ) {
        crate::trace_compiler!(
            "fir",
            "Pass 1 signature-default local publication failed: {declarations:?}",
        );
        return false;
    }
    for item in items {
        let mut sink = DefaultArgumentSink {
            index: &*index,
            store,
            missing_owner: false,
        };
        if let Err(error) = crate::fir::check_and_dispatch_signature_defaults_in_session(
            file,
            &info,
            source,
            item,
            &*index,
            &active,
            sources.origins_mut(),
            &mut sink,
            session,
        ) {
            crate::trace_compiler!(
                "fir",
                "Pass 1 signature-default FIR construction failed target={:?} target_anchor={:?} provider={:?} provider_anchor={:?} target_signature={} target_callable={}: {error:?}",
                item.target,
                index.declaration_anchor(item.target),
                item.provider,
                index.declaration_anchor(item.provider),
                index.signature(item.target).is_some(),
                index.callable_for_declaration(item.target).is_some(),
            );
            return false;
        }
        if sink.missing_owner {
            crate::trace_compiler!(
                "fir",
                "Pass 1 signature-default target has no surviving callable: {:?}",
                item.target,
            );
            return false;
        }
    }
    true
}

pub(super) fn from_checked_analysis(
    module: crate::fir::FrontendModule,
    bodies: crate::fir::BodyPartition,
    default_arguments: crate::fir::DefaultArgumentStore,
    files: &[File],
    types: &[Option<FrontendTypeInfo>],
    symbols: &mut FrontendSymbols,
) -> Option<StreamedPassState> {
    let selection = bodies.inline_check_selection(module.index(), files.len());
    let payload_roots = inline_payload_roots(&selection, module.index())?;
    let (mut index, mut inline_bodies, initial_defaults, mut sources) = module.into_parts();
    assert!(
        initial_defaults.is_empty(),
        "signature defaults are installed only after Pass-1 body preparation"
    );
    crate::trace_compiler!(
        "fir",
        "Pass 1 inline preparation inline_units={} ordinary_units={}",
        bodies.inline.len(),
        bodies.ordinary.len(),
    );
    let mut inline_owners = vec![Vec::new(); files.len()];
    for work in bodies.inline.units() {
        let Some(anchor) = index.declaration_anchor(work.declaration) else {
            crate::trace_compiler!(
                "fir",
                "Pass 1 inline work has no declaration anchor: {work:?}",
            );
            return None;
        };
        inline_owners[anchor.source.raw() as usize].push(work.declaration);
    }
    for (raw_source, owners) in inline_owners.iter().enumerate() {
        if owners.is_empty() {
            continue;
        }
        let source = crate::fir::SourceFileId::from_raw(raw_source as u32);
        let Some(file) = files.get(raw_source) else {
            crate::trace_compiler!("fir", "Pass 1 inline-local source {raw_source} is absent",);
            return None;
        };
        let Some(info) = types.get(raw_source).and_then(Option::as_ref) else {
            crate::trace_compiler!(
                "fir",
                "Pass 1 inline-local source {raw_source} has no checked type information",
            );
            return None;
        };
        if let Err(declarations) = crate::resolve::publish_checked_inline_local_signatures(
            file, source, symbols, info, &mut index, owners, None,
        ) {
            crate::trace_compiler!(
                "fir",
                "Pass 1 inline-local signature publication failed: {declarations:?}",
            );
            return None;
        }
    }
    let mut unexpected = UnexpectedOrdinaryInlineBody::default();
    let mut sessions = (0..files.len())
        .map(|_| crate::fir::BodyCheckSession::default())
        .collect::<Vec<_>>();
    for work in bodies.inline.units().iter().copied() {
        let anchor = index.declaration_anchor(work.declaration)?;
        let source = anchor.source;
        let file = files.get(source.raw() as usize)?;
        let info = types.get(source.raw() as usize).and_then(Option::as_ref)?;
        if let Err(error) = crate::fir::check_and_dispatch_body_in_session(
            file,
            info,
            source,
            work,
            &mut index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut unexpected,
            &mut sessions[source.raw() as usize],
        ) {
            crate::trace_compiler!(
                "fir",
                "Pass 1 inline FIR construction failed for {:?}: {error:?}",
                work.declaration,
            );
            return None;
        }
    }
    let mut nested = InlineNestedBodyCollector {
        roots: payload_roots,
        ..InlineNestedBodyCollector::default()
    };
    let nested_work = bodies
        .ordinary
        .units()
        .iter()
        .copied()
        .filter(|work| nested.roots.contains_key(&work.declaration))
        .collect::<Vec<_>>();
    for work in nested_work {
        let anchor = index.declaration_anchor(work.declaration)?;
        let source = anchor.source;
        let file = files.get(source.raw() as usize)?;
        let info = types.get(source.raw() as usize).and_then(Option::as_ref)?;
        if let Err(error) = crate::fir::check_and_dispatch_body_in_session(
            file,
            info,
            source,
            work,
            &mut index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut nested,
            &mut sessions[source.raw() as usize],
        ) {
            crate::trace_compiler!(
                "fir",
                "Pass 1 nested inline-payload FIR construction failed for {:?}: {error:?}",
                work.declaration,
            );
            return None;
        }
    }
    if !attach_inline_nested_bodies(&mut inline_bodies, nested) {
        return None;
    }
    finish(index, inline_bodies, sources, default_arguments, unexpected)
}

pub(super) fn streaming(
    module: crate::fir::FrontendModule,
    bodies: crate::fir::BodyPartition,
    default_arguments: crate::fir::DefaultArgumentStore,
    files: &mut [File],
    skip: &[bool],
    checked_count: usize,
    symbols: &mut FrontendSymbols,
    diags: &mut crate::diag::DiagSink,
) -> Option<StreamedPassState> {
    let selection = bodies.inline_check_selection(module.index(), files.len());
    let payload_roots = inline_payload_roots(&selection, module.index())?;
    let (mut index, mut inline_bodies, initial_defaults, mut sources) = module.into_parts();
    assert!(
        initial_defaults.is_empty(),
        "signature defaults are installed only after Pass-1 body preparation"
    );
    let mut inline_work = bodies.inline;
    let mut inline_owners = vec![Vec::new(); files.len()];
    for work in inline_work.units() {
        let anchor = index.declaration_anchor(work.declaration)?;
        inline_owners[anchor.source.raw() as usize].push(work.declaration);
    }
    let mut unexpected = UnexpectedOrdinaryInlineBody::default();
    let mut sessions = (0..files.len())
        .map(|_| crate::fir::BodyCheckSession::default())
        .collect::<Vec<_>>();

    // Sources without inline bodies have no remaining Pass-1 syntax consumer: signature defaults
    // were already checked and detached before compact headers were consumed. Drop the complete
    // legacy declaration view, not only its expression/statement arenas; active inline checking
    // below resolves every sibling declaration through the finalized semantic index.
    for (raw_source, owners) in inline_owners.iter().enumerate() {
        if owners.is_empty() {
            files[raw_source] = File::default();
        }
    }

    crate::trace_compiler!(
        "fir",
        "Pass 1 streaming inline preparation inline_units={} ordinary_units={}",
        inline_work.len(),
        bodies.ordinary.len(),
    );
    for (raw_source, owners) in inline_owners.iter().enumerate() {
        if owners.is_empty() {
            continue;
        }
        if raw_source >= checked_count || skip.get(raw_source).copied().unwrap_or(false) {
            crate::trace_compiler!(
                "fir",
                "Pass 1 selected inline source {raw_source} is not checkable",
            );
            return None;
        }
        let source = crate::fir::SourceFileId::from_raw(raw_source as u32);
        diags.set_file(raw_source as u32);
        if !owners.is_empty() {
            let selected_stable_bodies = selection.stable_bodies[raw_source].clone();
            crate::trace_compiler!(
                "fir",
                "Pass 1 retained stable bodies source={source:?} bodies={selected_stable_bodies:?}",
            );
            let active_roots = owners.iter().copied().collect();
            let active = match crate::fir::ActiveSourceDeclarations::bind_retained_fragments(
                &files[raw_source],
                source,
                &index,
                &active_roots,
                &selection.stable_bodies[raw_source],
            ) {
                Ok(active) => active,
                Err(error) => {
                    crate::trace_compiler!(
                        "fir",
                        "Pass 1 inline source binding failed source={source:?}: {error:?}",
                    );
                    return None;
                }
            };
            let info = crate::resolve::check_preinferred_inline_declarations_at_with_index(
                &files[raw_source],
                raw_source as u32,
                &selection.roots[raw_source],
                &selection.bodies[raw_source],
                &selected_stable_bodies,
                &active,
                &index,
                symbols,
                diags,
            );
            let file = files.get(raw_source)?;
            if let Err(declarations) = crate::resolve::publish_checked_inline_local_signatures(
                file,
                source,
                symbols,
                &info,
                &mut index,
                owners,
                Some(&active),
            ) {
                crate::trace_compiler!(
                    "fir",
                    "Pass 1 inline-local signature publication failed: {declarations:?}",
                );
                return None;
            }
            for work in inline_work.take_for_source(&index, source) {
                if let Err(error) = crate::fir::check_and_dispatch_active_body_in_session(
                    file,
                    &active,
                    &info,
                    source,
                    work,
                    &index,
                    sources.origins_mut(),
                    &mut inline_bodies,
                    &mut unexpected,
                    &mut sessions[raw_source],
                ) {
                    crate::trace_compiler!(
                        "fir",
                        "Pass 1 inline FIR construction failed for {:?}: {error:?}",
                        work.declaration,
                    );
                    return None;
                }
            }
            let nested_work = bodies
                .ordinary
                .units()
                .iter()
                .copied()
                .filter(|work| {
                    index
                        .declaration_anchor(work.declaration)
                        .is_some_and(|anchor| anchor.source == source)
                        && payload_roots.contains_key(&work.declaration)
                })
                .collect::<Vec<_>>();
            let mut nested = InlineNestedBodyCollector {
                roots: payload_roots.clone(),
                ..InlineNestedBodyCollector::default()
            };
            for work in nested_work {
                if let Err(error) = crate::fir::check_and_dispatch_active_body_in_session(
                    file,
                    &active,
                    &info,
                    source,
                    work,
                    &index,
                    sources.origins_mut(),
                    &mut inline_bodies,
                    &mut nested,
                    &mut sessions[raw_source],
                ) {
                    crate::trace_compiler!(
                        "fir",
                        "Pass 1 nested inline-payload FIR construction failed for {:?}: {error:?}",
                        work.declaration,
                    );
                    return None;
                }
            }
            if !attach_inline_nested_bodies(&mut inline_bodies, nested) {
                return None;
            }
        }
        // `info` is intentionally dropped here. Production never accumulates checked side tables
        // for more than the active inline source. Release the complete legacy declaration view at
        // the same boundary; later inline files see this source only through stable semantic data.
        files[raw_source] = File::default();
    }
    assert!(
        inline_work.is_empty(),
        "every selected inline body must be consumed with its source"
    );
    assert!(
        files.iter().all(|file| {
            file.decl_arena.is_empty() && file.expr_arena.is_empty() && file.stmt_arena.is_empty()
        }),
        "no legacy parser arena may survive Pass-1 inline/default preparation"
    );
    finish(index, inline_bodies, sources, default_arguments, unexpected)
}

fn finish(
    mut index: crate::fir::ResolvedModuleIndex,
    inline_bodies: crate::fir::InlineBodyStore,
    sources: crate::fir::SourceMap,
    default_arguments: crate::fir::DefaultArgumentStore,
    unexpected: UnexpectedOrdinaryInlineBody,
) -> Option<StreamedPassState> {
    assert_eq!(
        unexpected.0, 0,
        "Pass 1 may retain only semantically inline checked bodies"
    );
    index.release_source_coordinates();
    assert!(
        !index.retains_source_coordinates(),
        "Pass 2 must not retain Pass-1 source coordinates"
    );
    Some(StreamedPassState {
        module: crate::fir::FrontendModule::new(index, inline_bodies, default_arguments, sources),
        diagnostic_recovery: false,
    })
}
