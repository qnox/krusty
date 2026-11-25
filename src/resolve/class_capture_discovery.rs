use super::*;

#[derive(Clone)]
pub(super) struct ClassCapturePlan {
    /// One decision per method in the retained instance/enum-entry prefix. A shorter vector means
    /// the remaining suffix is irrelevant; `false` inside the vector means semantic checking may be
    /// skipped but the checker's ordered mutation summary must still advance past that method.
    pub(super) methods: Vec<bool>,
}

impl ClassCapturePlan {
    /// Refine one already-selected class without reintroducing a resolver-owned inventory of class
    /// fields. `declaration_targets` came from the generic `File::any_decl_expr` walk above; this
    /// method only asks the corresponding generic function seam which ordered method owns each
    /// target.
    ///
    /// Instance and enum-entry methods publish inferred returns into shared checker state. Retain
    /// the exact prefix through the last capture-bearing method, but semantically check within that
    /// prefix only methods that own a target or can publish such a return. A target in any other
    /// class region keeps the complete prefix because constructors, properties, init blocks, enum
    /// arguments may consume those published returns.
    fn for_class(
        file: &File,
        class: &ClassDecl,
        targets: &crate::fx_hash::HashSet<ExprId>,
        declaration_targets: &crate::fx_hash::HashSet<ExprId>,
    ) -> Self {
        let methods = class
            .methods
            .iter()
            .chain(
                class
                    .enum_entries
                    .iter()
                    .flat_map(|entry| entry.methods.iter()),
            )
            .collect::<Vec<_>>();
        let mut method_targets = crate::fx_hash::HashSet::default();
        let mut selected_methods = methods
            .iter()
            .map(|method| {
                record_function_capture_targets(file, method, targets, &mut method_targets)
            })
            .collect::<Vec<_>>();
        let mut method_check_prefix_len = selected_methods
            .iter()
            .rposition(|selected| *selected)
            .map_or(0, |index| index + 1);
        if declaration_targets
            .iter()
            .any(|target| !method_targets.contains(target))
        {
            method_check_prefix_len = methods.len();
        }
        selected_methods.truncate(method_check_prefix_len);
        for (selected, method) in selected_methods.iter_mut().zip(methods) {
            *selected |= method_may_publish_inferred_return(method);
        }

        Self {
            methods: selected_methods,
        }
    }
}

pub(super) struct CaptureDiscoveryScope {
    pub(super) declarations: crate::fx_hash::HashSet<DeclId>,
    pub(super) class_plans: crate::fx_hash::HashMap<DeclId, ClassCapturePlan>,
}

/// Anonymous-class bodies nested under an inline callable are ordinary emission bodies, but their
/// enclosing inline syntax was consumed in Pass 1. When one of those bodies is selected in
/// Pass 2, enter its classifier directly from stable declaration ownership and the capture/type-
/// parameter facts already published in Pass 1. This avoids reopening the inline body and avoids
/// retaining an ordinary AST or resolver side table across the pass boundary.
pub(super) fn selected_inline_owned_anonymous_classes(
    file: &File,
    index: &crate::fir::ResolvedModuleIndex,
    active: Option<&crate::fir::ActiveSourceDeclarations>,
    selected_bodies: &crate::fx_hash::HashSet<crate::fir::DeclarationId>,
    anonymous: &AnonymousLexicalClassScope,
) -> Vec<(DeclId, Vec<crate::fir::DeclarationId>)> {
    let mut selected = HashMap::<DeclId, Vec<crate::fir::DeclarationId>>::default();
    for &declaration in selected_bodies {
        let mut current = Some(declaration);
        let mut anonymous_class = None;
        let mut inline_owners = Vec::new();
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some(candidate) = current {
            if !seen.insert(candidate) {
                break;
            }
            let Some(candidate_anchor) = index.declaration_anchor(candidate) else {
                break;
            };
            // A parser-hoisted anonymous/local classifier can have an early ancestry alias and a
            // later published semantic declaration. Both bind the same active parser node, but only
            // the latter has the repaired lexical owner. Follow that stable alias before walking
            // outward; otherwise nested work appears orphaned before reaching its inline owner.
            if candidate_anchor.kind == crate::fir::DeclarationKind::Classifier {
                if let Some(canonical) =
                    active.and_then(|active| active.same_parser_local_class(candidate, index))
                {
                    current = Some(canonical);
                    continue;
                }
            }
            if candidate_anchor.kind == crate::fir::DeclarationKind::Classifier
                && anonymous_class.is_none()
            {
                let transient = active.and_then(|active| {
                    active
                        .class(file, candidate)
                        .map(|(transient, _)| transient)
                });
                let retained_local = active.is_none()
                    && index.declaration_header(candidate).is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    });
                let stable_anonymous = index.declaration_header(candidate).is_some_and(|header| {
                    header
                        .flags
                        .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
                });
                anonymous_class = transient.filter(|transient| {
                    stable_anonymous || retained_local || anonymous.declarations.contains(transient)
                });
            }
            if index
                .callable_for_declaration(candidate)
                .is_some_and(|callable| callable.is_inline())
            {
                inline_owners.push(candidate);
            }
            current = index
                .declaration_header(candidate)
                .and_then(|header| header.owner)
                .or(candidate_anchor.owner);
        }
        if let Some(class) = anonymous_class.filter(|_| !inline_owners.is_empty()) {
            let owners = selected.entry(class).or_default();
            // Ownership was walked from the selected body outwards. Publish lexical type-
            // parameter rungs outermost-first so an inner declaration shadows normally.
            for owner in inline_owners.into_iter().rev() {
                if !owners.contains(&owner) {
                    owners.push(owner);
                }
            }
        }
    }
    let mut selected = selected.into_iter().collect::<Vec<_>>();
    selected.sort_by_key(|(declaration, _)| declaration.0);
    selected
}

/// Capture discovery needs lexical state from the declaration containing each anonymous-object
/// construction, but declarations are checked in isolated top-level scopes. Find those owners once
/// so the scratch checker does not re-check every unrelated declaration in the file.
pub(super) fn capture_discovery_scope(file: &File) -> CaptureDiscoveryScope {
    let targets = file
        .anonymous_object_classes
        .keys()
        .copied()
        .collect::<crate::fx_hash::HashSet<_>>();
    let mut discovered =
        crate::fx_hash::HashSet::with_capacity_and_hasher(targets.len(), Default::default());
    let mut class_plans = crate::fx_hash::HashMap::default();
    let declarations = file
        .decls
        .iter()
        .copied()
        .filter(|declaration| {
            let mut roots = Vec::new();
            file.any_decl_expr(*declaration, &mut |expression| {
                roots.push(expression);
                false
            });
            let mut declaration_targets = crate::fx_hash::HashSet::default();
            let owns_target =
                record_expression_targets(file, &targets, roots, &mut declaration_targets);
            if owns_target {
                if let Decl::Class(class) = file.decl(*declaration) {
                    class_plans.insert(
                        *declaration,
                        ClassCapturePlan::for_class(file, class, &targets, &declaration_targets),
                    );
                }
                discovered.extend(declaration_targets);
            }
            owns_target
        })
        // Parser-hoisted local classifiers are entered only while their enclosing executable is
        // checked. Selecting the hoisted declaration itself would be a dead root: the file-level
        // checker deliberately skips it, and an anonymous construction in one of its initializer
        // bodies would never run capture discovery. Follow the parser's explicit lexical edge to
        // the bounded source declaration that actually enters the local classifier. This is an
        // active-AST ownership edge, not a retained source-range locator.
        .map(|declaration| {
            file.local_class_enclosing_declarations
                .get(&declaration)
                .copied()
                .unwrap_or(declaration)
        })
        .collect::<crate::fx_hash::HashSet<_>>();
    // Keep capture analysis conservative as the AST evolves. If a parser change retains an
    // anonymous construction outside the generic declaration-root inventory, retain the old
    // full-file semantic pass rather than silently treating that construction as capture-free.
    let complete = discovered.len() == targets.len();
    CaptureDiscoveryScope {
        declarations: if complete {
            declarations
        } else {
            file.decls.iter().copied().collect()
        },
        // An unknown expression-bearing parser field invalidates not only declaration selection but
        // every narrower class plan. An empty map makes callers preserve the full semantic walk.
        class_plans: if complete {
            class_plans
        } else {
            crate::fx_hash::HashMap::default()
        },
    }
}

#[derive(Default, Clone)]
pub(super) struct AnonymousLexicalClassScope {
    pub(super) owners: HashMap<DeclId, DeclId>,
    pub(super) declarations: crate::fx_hash::HashSet<DeclId>,
}

impl AnonymousLexicalClassScope {
    /// The declaration followed by its structurally recorded anonymous-object owners, nearest first.
    /// This is the single graph walk used by signature collection, pre-inference, and the main checker;
    /// central cycle protection prevents those phases from disagreeing on malformed/recovered input.
    pub(super) fn declaration_chain(&self, declaration: DeclId) -> Vec<DeclId> {
        let mut chain = Vec::new();
        let mut visited = crate::fx_hash::HashSet::default();
        let mut current = Some(declaration);
        while let Some(candidate) = current {
            if !visited.insert(candidate) {
                break;
            }
            chain.push(candidate);
            current = self.owners.get(&candidate).copied();
        }
        chain
    }

    pub(super) fn belongs_to_anonymous_subtree(&self, declaration: DeclId) -> bool {
        self.declaration_chain(declaration)
            .into_iter()
            .any(|candidate| self.declarations.contains(&candidate))
    }
}

pub(super) fn anonymous_lexical_class_scope(file: &File) -> AnonymousLexicalClassScope {
    if file.anonymous_object_classes.is_empty() {
        return AnonymousLexicalClassScope::default();
    }
    let mut owners = HashMap::default();
    let declarations = file
        .anonymous_object_classes
        .values()
        .copied()
        .collect::<crate::fx_hash::HashSet<_>>();
    let targets = file
        .anonymous_object_classes
        .keys()
        .copied()
        .collect::<crate::fx_hash::HashSet<_>>();
    for &owner in &file.decls {
        if !matches!(file.decl(owner), Decl::Class(_)) {
            continue;
        }
        let mut roots = Vec::new();
        file.any_decl_expr(owner, &mut |expression| {
            // A compact Pass-1 file preserves declaration headers but replaces every discarded
            // ordinary body root with an out-of-arena sentinel. Anonymous ownership is needed only
            // for retained constructions, whose complete subtree is dense in the compact arena.
            if (expression.0 as usize) < file.expr_arena.len() {
                roots.push(expression);
            }
            false
        });
        let mut constructions = crate::fx_hash::HashSet::default();
        record_expression_targets(file, &targets, roots, &mut constructions);
        for construction in constructions {
            if let Some(&anonymous) = file.anonymous_object_classes.get(&construction) {
                owners.entry(anonymous).or_insert(owner);
            }
        }
    }
    // Named classifiers nested inside an anonymous object are parser-hoisted too. Add their
    // explicit `inner_of` edges when that chain reaches an anonymous declaration, so a use in the
    // named descendant makes the anonymous owner capture the value it must later expose through its
    // generated field. Keep `declarations` restricted to actual anonymous objects; callers use that
    // set to decide generated-name semantics.
    let names = file
        .decls
        .iter()
        .filter_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class) => Some((class.name.as_str(), declaration)),
            Decl::Fun(_) | Decl::Property(_) => None,
        })
        .collect::<HashMap<_, _>>();
    loop {
        let mut changed = false;
        for &declaration in &file.decls {
            let Decl::Class(class) = file.decl(declaration) else {
                continue;
            };
            let Some(owner) = class
                .inner_of
                .as_deref()
                .and_then(|owner| names.get(owner))
                .copied()
            else {
                continue;
            };
            if (declarations.contains(&owner) || owners.contains_key(&owner))
                && !owners.contains_key(&declaration)
            {
                owners.insert(declaration, owner);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    AnonymousLexicalClassScope {
        owners,
        declarations,
    }
}

/// Classifier roots visible from a declaration in nearest-first order. Ordinary nested declarations
/// may derive their source parents from their hoisted internal name; anonymous declarations may not —
/// their generated `$` name is exact, and their parents come only from the structural ownership map.
pub(super) fn declaration_lexical_class_names(
    file: &File,
    declaration: DeclId,
    scope: &AnonymousLexicalClassScope,
    mut exists: impl FnMut(TypeName) -> bool,
) -> Vec<TypeName> {
    let mut classes = Vec::new();
    for candidate in scope.declaration_chain(declaration) {
        if let Decl::Class(class) = file.decl(candidate) {
            let internal = type_name(&class_internal(file, &class.name));
            if scope.declarations.contains(&candidate) {
                if !classes.contains(&internal) {
                    classes.push(internal);
                }
            } else {
                for lexical in crate::symbol_resolver::lexical_enclosing_classifier_names(
                    internal,
                    &mut exists,
                ) {
                    if !classes.contains(&lexical) {
                        classes.push(lexical);
                    }
                }
            }
        }
    }
    classes
}

pub(super) fn discover_anonymous_object_captures_at(
    file: &File,
    file_index: u32,
    syms: &mut SymbolTable,
    selected_declarations: Option<&crate::fx_hash::HashSet<Span>>,
    selected_body_declarations: Option<&crate::fx_hash::HashSet<Span>>,
) -> HashMap<DeclId, Vec<AnonymousObjectCapture>> {
    let declarations = file
        .anonymous_object_classes
        .values()
        .copied()
        .collect::<crate::fx_hash::HashSet<_>>();
    if declarations.is_empty() {
        return HashMap::default();
    }
    if syms.pass_one_symbols().is_some_and(|symbols| {
        declarations.iter().all(|declaration| {
            symbols
                .anonymous_object_capture_discovered
                .contains(&(file_index, *declaration))
        })
    }) {
        let symbols = syms
            .pass_one_symbols()
            .expect("legacy capture discovery requires Pass-1 symbols");
        return declarations
            .into_iter()
            .filter_map(|declaration| {
                symbols
                    .anonymous_object_captures
                    .get(&(file_index, declaration))
                    .cloned()
                    .map(|captures| (declaration, captures))
            })
            .collect();
    }

    let mut scratch = DiagSink::new();
    let info = check_file_at_impl_mode_with_index(IndexedFileCheck {
        file,
        file_index,
        source_files: None,
        syms,
        resolved_index: None,
        diags: &mut scratch,
        captures: CaptureDiscovery::Scratch,
        selected_declarations,
        selected_body_declarations,
        active_declarations: None,
        selected_stable_roots: None,
        selected_stable_bodies: None,
        fragment: SourceFragmentMode::Complete,
        streamed_cache: None,
    });
    let mut discovered = info.anonymous_object_captures_by_class;
    if selected_declarations.is_none() {
        for declaration in declarations {
            discovered.entry(declaration).or_default();
        }
    }
    if let Some(symbols) = syms.pass_one_symbols_mut() {
        install_anonymous_object_captures(symbols, file_index, discovered.clone());
    }
    discovered
}
