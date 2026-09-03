//! Parser-native declaration bindings for one active source.
//!
//! Pass 1 retains only the stable declaration stream. When Pass 2 reparses a source, this module
//! zips that fresh parser stream with the stable identities and immediately converts it to arena
//! references. Stable source ranges are deliberately not consulted: they remain diagnostic data,
//! never body locators.

use crate::ast::{
    ClassDecl, ClassInit, Decl, DeclId, File, FunBody, FunDecl, PropDecl, SecondaryCtor,
};
use crate::diag::Span;

use super::{
    extract_file_stubs, DeclarationFlags, DeclarationId, DeclarationIds, DeclarationKind,
    LookupNames, ResolvedModuleIndex, SourceFileId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivePropertyRef {
    TopLevel(DeclId),
    ClassBody {
        class: DeclId,
        index: u32,
    },
    EnumEntry {
        class: DeclId,
        entry: u32,
        index: u32,
    },
    ConstructorParameter {
        class: DeclId,
        index: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveDeclarationRef {
    Function(DeclId),
    ClassMethod {
        class: DeclId,
        index: u32,
    },
    EnumEntryMethod {
        class: DeclId,
        entry: u32,
        index: u32,
    },
    Property(ActivePropertyRef),
    Classifier(DeclId),
    PrimaryConstructor {
        class: DeclId,
    },
    SecondaryConstructor {
        class: DeclId,
        index: u32,
    },
    Accessor {
        property: ActivePropertyRef,
        setter: bool,
    },
    ClassInitializer {
        class: DeclId,
        index: u32,
    },
    EnumEntryInitializer {
        class: DeclId,
        entry: u32,
        index: u32,
    },
    EnumEntry {
        class: DeclId,
        index: u32,
    },
    Script,
    TypeAlias,
    Generated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActiveSourceBindingError {
    InventoryLength,
    InventoryShape(DeclarationId),
    MissingParserDeclaration(DeclarationId),
}

/// Declaration identities rebound to the parser arenas of exactly one active Pass-2 source.
///
/// The values are parser indices, not source coordinates. This object cannot outlive the active
/// `File` by API convention and is dropped with it before the next bounded source is parsed.
#[derive(Clone, Debug)]
pub(crate) struct ActiveSourceDeclarations {
    declarations: Vec<Option<ActiveDeclarationRef>>,
    /// Reverse binding for the parser declaration inventory extracted from the one live unit.
    /// These are arena-local IDs and the table is dropped together with that unit.
    parser_declarations: Vec<Option<DeclarationId>>,
}

fn alias_default_binding(
    active: &mut ActiveSourceDeclarations,
    index: &ResolvedModuleIndex,
    provider: DeclarationId,
    target: DeclarationId,
    replace_target: bool,
) -> Result<(), ActiveSourceBindingError> {
    let provider_anchor = index
        .declaration_anchor(provider)
        .ok_or(ActiveSourceBindingError::InventoryShape(provider))?;
    let target_anchor = index
        .declaration_anchor(target)
        .ok_or(ActiveSourceBindingError::InventoryShape(target))?;
    if provider_anchor.kind != target_anchor.kind {
        return Err(ActiveSourceBindingError::InventoryShape(target));
    }
    let binding = active
        .binding(provider)
        .ok_or(ActiveSourceBindingError::MissingParserDeclaration(provider))?;
    let slot = active
        .declarations
        .get_mut(target.raw() as usize)
        .ok_or(ActiveSourceBindingError::InventoryShape(target))?;
    if !replace_target && slot.is_some_and(|existing| existing != binding) {
        return Err(ActiveSourceBindingError::InventoryShape(target));
    }
    *slot = Some(binding);
    Ok(())
}

/// Sequential cursor over the stable declaration-header stream for one Pass-2 source.
///
/// This cursor is created after reparsing starts and advances only in response to parser units. It
/// contains no body inventory and cannot select or seek to source syntax.
#[derive(Debug)]
pub(crate) struct ActiveSourceCursor {
    declarations: Box<[DeclarationId]>,
    next: usize,
}

impl ActiveSourceCursor {
    pub(crate) fn new(source: SourceFileId, index: &ResolvedModuleIndex) -> Self {
        let mut declarations = index
            .source_inventory(source)
            .iter()
            .copied()
            .filter(|declaration| {
                index
                    .declaration_anchor(*declaration)
                    .is_some_and(|anchor| anchor.kind != DeclarationKind::TypeAlias)
            })
            .collect::<Vec<_>>();
        declarations.sort_by_key(|declaration| {
            (
                index.source_order(*declaration).unwrap_or(u32::MAX),
                *declaration,
            )
        });
        Self {
            declarations: declarations.into_boxed_slice(),
            next: 0,
        }
    }

    pub(crate) fn bind_next(
        &mut self,
        file: &File,
        source: SourceFileId,
        index: &ResolvedModuleIndex,
    ) -> Result<ActiveSourceDeclarations, ActiveSourceBindingError> {
        let mut parser_ids = DeclarationIds::default();
        let mut parser_names = LookupNames::default();
        let parser_len = extract_file_stubs(file, source, &mut parser_ids, &mut parser_names)
            .into_iter()
            .filter(|stub| stub.kind != DeclarationKind::TypeAlias)
            .count();
        let end = self
            .next
            .checked_add(parser_len)
            .filter(|end| *end <= self.declarations.len())
            .ok_or(ActiveSourceBindingError::InventoryLength)?;
        let stable = &self.declarations[self.next..end];
        let roots = stable
            .iter()
            .copied()
            .filter(|declaration| {
                index
                    .declaration_anchor(*declaration)
                    .is_some_and(|anchor| anchor.owner.is_none())
            })
            .collect::<std::collections::HashSet<_>>();
        let active = ActiveSourceDeclarations::bind_selected(
            file,
            source,
            index,
            stable,
            Some(&roots),
            None,
            None,
        )?;
        self.next = end;
        Ok(active)
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.next == self.declarations.len()
    }
}

impl ActiveSourceDeclarations {
    /// Bind a whole-file syntax arena retained by a same-parse caller to the complete stable
    /// declaration stream. Production Pass 2 binds one bounded parser unit at a time through
    /// [`ActiveSourceCursor`]; this adapter exists for Pass-1 inline checking and focused tests
    /// whose syntax arena is already live. The returned table is still transient and contains
    /// parser indices rather than source coordinates.
    #[cfg(test)]
    pub(crate) fn bind_complete_source(
        file: &File,
        source: SourceFileId,
        index: &ResolvedModuleIndex,
    ) -> Result<Self, ActiveSourceBindingError> {
        let mut cursor = ActiveSourceCursor::new(source, index);
        let active = cursor.bind_next(file, source, index)?;
        if !cursor.is_finished() {
            return Err(ActiveSourceBindingError::InventoryLength);
        }
        Ok(active)
    }

    /// Bind the stable declarations participating in retained Pass-1 executable fragments to the
    /// parser arena that is live right now. The binding is transient: checked FIR and stable
    /// signatures consume it before the arena is released, and no parser identity crosses into
    /// Pass 2.
    pub(crate) fn bind_retained_fragments(
        file: &File,
        source: SourceFileId,
        index: &ResolvedModuleIndex,
        roots: &std::collections::HashSet<DeclarationId>,
        bodies: &std::collections::HashSet<DeclarationId>,
    ) -> Result<Self, ActiveSourceBindingError> {
        let stable = index
            .source_inventory(source)
            .iter()
            .copied()
            .filter(|declaration| {
                index
                    .declaration_anchor(*declaration)
                    .is_some_and(|anchor| anchor.kind != DeclarationKind::TypeAlias)
            })
            .collect::<Vec<_>>();
        let mut selected = roots
            .iter()
            .chain(bodies)
            .copied()
            .collect::<std::collections::HashSet<_>>();
        // A retained executable fragment owns every local declaration nested in that fragment.
        // The checker may enter an anonymous/local classifier while checking an inline body or a
        // default expression and must then publish its inferred members against their stable
        // headers. Binding only the provider and its ancestors leaves those descendants with
        // parser-arena identities and makes publication impossible. This expands semantic owner
        // edges only; it neither selects an ordinary body for checking nor retains syntax.
        let retained_roots = selected.clone();
        for declaration in stable.iter().copied() {
            let mut current = Some(declaration);
            while let Some(candidate) = current {
                if retained_roots.contains(&candidate) {
                    selected.insert(declaration);
                    break;
                }
                current = index
                    .local_classifier_lexical_root(candidate)
                    .or_else(|| {
                        index
                            .declaration_header(candidate)
                            .and_then(|header| header.owner)
                    })
                    .or_else(|| {
                        index
                            .declaration_anchor(candidate)
                            .and_then(|anchor| anchor.owner)
                    });
            }
        }
        loop {
            let before = selected.len();
            for declaration in selected.iter().copied().collect::<Vec<_>>() {
                if let Some(owner) = index
                    .declaration_header(declaration)
                    .and_then(|header| header.owner)
                    .or_else(|| {
                        index
                            .declaration_anchor(declaration)
                            .and_then(|anchor| anchor.owner)
                    })
                {
                    selected.insert(owner);
                }
            }
            if selected.len() == before {
                break;
            }
        }
        Self::bind_selected(file, source, index, &stable, None, None, Some(&selected))
    }

    /// Bind only signature-owned defaults and the stable declaration headers that lexically own
    /// them. Pass 1 may already have released unrelated ordinary expressions, so those declarations
    /// must neither participate in structural validation nor become active syntax bindings.
    pub(crate) fn bind_defaults(
        file: &File,
        source: SourceFileId,
        index: &ResolvedModuleIndex,
        roots: &std::collections::HashSet<DeclarationId>,
        providers: &std::collections::HashSet<DeclarationId>,
        mappings: &[super::DefaultArgumentProvider],
    ) -> Result<Self, ActiveSourceBindingError> {
        let mut active = Self::bind_retained_fragments(file, source, index, roots, providers)?;
        // These aliases exist only while checking bounded Pass-1 defaults. Actualization replaces a
        // declaration and therefore redirects the structurally paired lexical owner chain. An
        // inherited override keeps the provider's lexical owner and redirects only the callable
        // whose target-owned FIR is being built.
        for work in mappings {
            match work.relation {
                super::DefaultArgumentRelation::SameDeclaration => {
                    if work.provider != work.target {
                        return Err(ActiveSourceBindingError::InventoryShape(work.target));
                    }
                }
                super::DefaultArgumentRelation::ActualizedDeclaration => {
                    let mut provider = Some(work.provider);
                    let mut target = Some(work.target);
                    while let (Some(provider_id), Some(target_id)) = (provider, target) {
                        alias_default_binding(&mut active, index, provider_id, target_id, true)?;
                        provider = index
                            .declaration_anchor(provider_id)
                            .and_then(|anchor| anchor.owner);
                        target = index
                            .declaration_anchor(target_id)
                            .and_then(|anchor| anchor.owner);
                    }
                    if provider.is_some() || target.is_some() {
                        return Err(ActiveSourceBindingError::InventoryShape(work.target));
                    }
                }
                super::DefaultArgumentRelation::InheritedOverride => {
                    alias_default_binding(&mut active, index, work.provider, work.target, true)?;
                }
            }
        }
        Ok(active)
    }

    fn bind_selected(
        file: &File,
        source: SourceFileId,
        index: &ResolvedModuleIndex,
        stable: &[DeclarationId],
        unit_roots: Option<&std::collections::HashSet<DeclarationId>>,
        root_bodies: Option<&std::collections::HashSet<DeclarationId>>,
        selected: Option<&std::collections::HashSet<DeclarationId>>,
    ) -> Result<Self, ActiveSourceBindingError> {
        let mut parser_ids = DeclarationIds::default();
        let mut parser_names = LookupNames::default();
        let mut parser_stubs = extract_file_stubs(file, source, &mut parser_ids, &mut parser_names);
        // A source typealias has no ordinary executable unit. The streaming parser can accumulate
        // one immediately before the next declaration callback, while Pass 2 receives its complete
        // compact header environment separately. It therefore participates in neither active AST
        // binding nor body-group cardinality.
        parser_stubs.retain(|stub| stub.kind != DeclarationKind::TypeAlias);
        // Both sides expose the same semantic declaration stream. Join those streams directly;
        // parser-arena insertion ids and source ranges are deliberately irrelevant. In particular,
        // releasing an ordinary accessor body may change only its diagnostic fallback span, never
        // its getter/setter position in this stream.
        let mut stable = stable.to_vec();
        stable.sort_by_key(|declaration| index.source_order(*declaration).unwrap_or(u32::MAX));
        if stable.len() != parser_stubs.len() {
            crate::trace_compiler!(
                "fir",
                "active inventory length mismatch source={source:?} stable={} parser={} stable_ids={stable:?} parser_stubs={parser_stubs:?}",
                stable.len(),
                parser_stubs.len(),
            );
            return Err(ActiveSourceBindingError::InventoryLength);
        }

        let mut parser_to_stable = vec![None; parser_ids.len()];
        for (&stable, parser) in stable.iter().zip(&parser_stubs) {
            if selected.is_some_and(|selected| !selected.contains(&stable)) {
                continue;
            }
            let slot = parser_to_stable
                .get_mut(parser.id.raw() as usize)
                .ok_or_else(|| {
                    crate::trace_compiler!(
                        "fir",
                        "active parser id outside declaration arena stable={stable:?} parser={:?} parser_ids={} stubs={parser_stubs:?}",
                        parser.id,
                        parser_ids.len(),
                    );
                    ActiveSourceBindingError::InventoryShape(stable)
                })?;
            if slot.is_some_and(|prior| prior != stable) {
                crate::trace_compiler!(
                    "fir",
                    "active parser declaration bound twice stable={stable:?} prior={slot:?} parser={:?} stubs={parser_stubs:?}",
                    parser.id,
                );
                return Err(ActiveSourceBindingError::InventoryShape(stable));
            }
            *slot = Some(stable);
        }
        bind_auxiliary_parser_declarations(source, index, &parser_ids, &mut parser_to_stable);

        for (&stable, parser) in stable.iter().zip(&parser_stubs) {
            if selected.is_some_and(|selected| !selected.contains(&stable)) {
                continue;
            }
            let stable_anchor = index
                .declaration_anchor(stable)
                .ok_or(ActiveSourceBindingError::InventoryShape(stable))?;
            let parser_anchor = parser_ids
                .anchor(parser.id)
                .ok_or(ActiveSourceBindingError::InventoryShape(stable))?;
            let parser_owner = parser_anchor.owner.and_then(|owner| {
                parser_to_stable
                    .get(owner.raw() as usize)
                    .and_then(|owner| *owner)
            });
            let unit_local_sibling = unit_roots.is_some_and(|roots| {
                roots.contains(&stable) || stable_anchor.kind == DeclarationKind::Classifier
            });
            let parser_name = parser.lookup_name.and_then(|name| parser_names.get(name));
            let stable_name = index.declaration_name(stable);
            let stable_header = index.declaration_header(stable);
            let generated_local_classifier_name = stable_header.is_some_and(|header| {
                header.kind == DeclarationKind::Classifier
                    && header.flags.has(DeclarationFlags::LOCAL_CLASS)
            });
            // The whole-file Pass-1 parser hoists a statement-local classifier into the file
            // declaration arena, so its transient parser anchor has no owner. The compact header
            // has already repaired that semantic owner while both source structures were live.
            // Joining the selected stable identity to this live parser declaration is therefore
            // allowed to differ only on that owner edge; descendants bind through the classifier
            // normally. The transient binding is consumed before Pass 2.
            // Retained-fragment syntax may omit the ordinary property initializer/accessor that
            // originally introduced an anonymous classifier while retaining an inline descendant
            // body from that classifier. Re-extracting the reduced AST can then recover only a
            // wider classifier owner (or no owner). The compact header already captured the exact
            // executable owner while the complete Pass-1 unit was live, so retained-fragment
            // binding must trust that stable edge for every local classifier. Bounded Pass 2 still
            // requires exact owner equality because its ordinary unit is complete.
            let pass_one_hoisted_local_classifier =
                unit_roots.is_none() && generated_local_classifier_name;
            // Actualization deliberately removes the expect header from the semantic index while
            // retaining its stable anchor as a default-expression provider. Its parser spelling is
            // therefore unavailable from `declaration_name`; structural kind/owner/sibling checks
            // below still bind it exactly. A statement-local classifier's parser-generated suffix
            // contains an arena index, which necessarily changes when bounded Pass 2 resets the
            // arena for its unit. Its stable identity is the selected declaration's structural
            // position, never that temporary spelling. Ordinary surviving headers compare names.
            if stable_header.is_some()
                && !generated_local_classifier_name
                && stable_name != parser_name
            {
                crate::trace_compiler!(
                    "fir",
                    "active inventory name mismatch stable={stable:?} stable_name={stable_name:?} parser={:?} parser_name={parser_name:?}",
                    parser.id,
                );
                return Err(ActiveSourceBindingError::InventoryShape(stable));
            }
            if unit_roots.is_some_and(|roots| roots.contains(&stable)) {
                // An actualized-away expect deliberately has an anchor in the stable declaration
                // stream but no semantic header. Sequential Pass 2 still consumes and drops that
                // parser unit; there is simply no body to publish from it.
                if let Some(stable_header) = stable_header {
                    if stable_header.flags.has(DeclarationFlags::EXPECT)
                        != parser.flags.has(DeclarationFlags::EXPECT)
                        || (root_bodies.is_some_and(|bodies| bodies.contains(&stable))
                            && parser.body.is_none())
                    {
                        crate::trace_compiler!(
                            "fir",
                            "active root body/expect mismatch stable={stable:?} stable_flags={:?} parser={parser:?} root_bodies={root_bodies:?}",
                            stable_header.flags,
                        );
                        return Err(ActiveSourceBindingError::InventoryShape(stable));
                    }
                }
            }
            if stable_anchor.source != source
                || stable_anchor.kind != parser_anchor.kind
                || (!unit_local_sibling && stable_anchor.sibling != parser_anchor.sibling)
                || (!pass_one_hoisted_local_classifier && stable_anchor.owner != parser_owner)
            {
                crate::trace_compiler!(
                    "fir",
                    "active inventory shape mismatch stable={stable:?} stable_anchor={stable_anchor:?} parser={:?} parser_anchor={parser_anchor:?} parser_owner={parser_owner:?}",
                    parser.id,
                );
                return Err(ActiveSourceBindingError::InventoryShape(stable));
            }
        }

        let mut parser_flags = vec![DeclarationFlags::default(); parser_ids.len()];
        for stub in &parser_stubs {
            let Some(flags) = parser_flags.get_mut(stub.id.raw() as usize) else {
                crate::trace_compiler!(
                    "fir",
                    "active stub id outside flags arena stub={stub:?} parser_ids={}",
                    parser_ids.len(),
                );
                return Err(ActiveSourceBindingError::InventoryShape(stub.id));
            };
            *flags = stub.flags;
        }
        let mut parser_bindings = vec![None; parser_ids.len()];
        loop {
            let mut changed = false;
            for raw in 0..parser_ids.len() {
                if parser_bindings.get(raw).is_some_and(Option::is_some) {
                    continue;
                }
                let parser = DeclarationId::from_raw(
                    u32::try_from(raw).expect("active parser declarations exceed packed identity"),
                );
                let Some(anchor) = parser_ids.anchor(parser) else {
                    continue;
                };
                let owner = match anchor.owner {
                    Some(owner) => {
                        let Some(owner) = parser_bindings
                            .get(owner.raw() as usize)
                            .and_then(|binding| *binding)
                        else {
                            continue;
                        };
                        Some(owner)
                    }
                    None => None,
                };
                let Some(binding) =
                    active_binding(file, anchor.kind, parser_flags[raw], anchor.sibling, owner)
                else {
                    continue;
                };
                parser_bindings[raw] = Some(binding);
                changed = true;
            }
            if !changed {
                break;
            }
        }

        let parser_declarations = parser_to_stable.clone();
        let mut declarations = vec![None; index.declaration_count()];
        for (raw, stable) in parser_to_stable.into_iter().enumerate() {
            let Some(stable) = stable else {
                continue;
            };
            let Some(binding) = parser_bindings.get(raw).and_then(|binding| *binding) else {
                return Err(ActiveSourceBindingError::MissingParserDeclaration(stable));
            };
            let stable_slot = declarations
                .get_mut(stable.raw() as usize)
                .ok_or_else(|| {
                    crate::trace_compiler!(
                        "fir",
                        "active stable id outside declaration index stable={stable:?} declaration_count={}",
                        index.declaration_count(),
                    );
                    ActiveSourceBindingError::InventoryShape(stable)
                })?;
            if stable_slot.is_some_and(|prior| prior != binding) {
                crate::trace_compiler!(
                    "fir",
                    "active stable declaration has conflicting parser bindings stable={stable:?} parser_raw={raw} parser_anchor={:?} prior={stable_slot:?} next={binding:?}",
                    parser_ids.anchor(DeclarationId::from_raw(
                        u32::try_from(raw).expect("parser declaration overflow"),
                    )),
                );
                return Err(ActiveSourceBindingError::InventoryShape(stable));
            }
            *stable_slot = Some(binding);
        }
        Ok(Self {
            declarations,
            parser_declarations,
        })
    }

    /// Discover executable bodies from the parser unit that is live now. No body identity or body
    /// presence fact from Pass 1 participates in this enumeration.
    pub(crate) fn ordinary_body_work(
        &self,
        file: &File,
        source: SourceFileId,
        index: &ResolvedModuleIndex,
    ) -> Result<Vec<super::BodyWorkItem>, ActiveSourceBindingError> {
        let mut parser_ids = DeclarationIds::default();
        let mut parser_names = LookupNames::default();
        let parser_stubs = extract_file_stubs(file, source, &mut parser_ids, &mut parser_names);
        let retained_by_inline_owner = |declaration: DeclarationId| {
            let mut current = Some(declaration);
            let mut seen = std::collections::HashSet::new();
            while let Some(candidate) = current {
                if !seen.insert(candidate) {
                    return false;
                }
                if index
                    .callable_for_declaration(candidate)
                    .is_some_and(super::ResolvedCallableHeader::is_inline)
                {
                    return true;
                }
                current = index
                    .local_classifier_lexical_root(candidate)
                    .or_else(|| {
                        index
                            .declaration_header(candidate)
                            .and_then(|header| header.owner)
                    })
                    .or_else(|| {
                        index
                            .declaration_anchor(candidate)
                            .and_then(|anchor| anchor.owner)
                    });
            }
            false
        };
        let mut work = parser_stubs
            .into_iter()
            .filter_map(|stub| stub.body.map(|kind| (stub, kind)))
            .filter_map(|(stub, kind)| {
                self.parser_declarations
                    .get(stub.id.raw() as usize)
                    .and_then(|declaration| *declaration)
                    .map(|declaration| (declaration, kind))
            })
            .filter(|(declaration, _)| index.declaration_header(*declaration).is_some())
            .filter(|(declaration, _)| !retained_by_inline_owner(*declaration))
            .map(|(declaration, kind)| {
                if index.declaration_anchor(declaration).is_none() {
                    return Err(ActiveSourceBindingError::InventoryShape(declaration));
                }
                Ok(super::BodyWorkItem {
                    declaration,
                    owner: super::BodyOwnerId::from_raw(declaration.raw()),
                    kind,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        work.sort_by_key(|unit| {
            self.span(file, unit.declaration)
                .map_or((u32::MAX, u32::MAX, unit.declaration), |span| {
                    (span.lo, span.hi, unit.declaration)
                })
        });
        Ok(work)
    }

    fn binding(&self, declaration: DeclarationId) -> Option<ActiveDeclarationRef> {
        self.declarations
            .get(declaration.raw() as usize)
            .and_then(|binding| *binding)
    }

    pub(crate) fn same_parser_declaration(
        &self,
        left: DeclarationId,
        right: DeclarationId,
    ) -> bool {
        self.binding(left)
            .is_some_and(|left| Some(left) == self.binding(right))
    }

    pub(crate) fn classifier_declaration(&self, parser: DeclId) -> Option<DeclarationId> {
        self.declarations
            .iter()
            .enumerate()
            .find_map(|(raw, binding)| {
                (*binding == Some(ActiveDeclarationRef::Classifier(parser))).then(|| {
                    DeclarationId::from_raw(
                        u32::try_from(raw)
                            .expect("active declaration table exceeds packed identity"),
                    )
                })
            })
    }

    pub(crate) fn canonical_classifier_declaration(
        &self,
        parser: DeclId,
        index: &ResolvedModuleIndex,
    ) -> Option<DeclarationId> {
        self.declarations
            .iter()
            .enumerate()
            .filter_map(|(raw, binding)| {
                (*binding == Some(ActiveDeclarationRef::Classifier(parser))).then(|| {
                    DeclarationId::from_raw(
                        u32::try_from(raw)
                            .expect("active declaration table exceeds packed identity"),
                    )
                })
            })
            .max_by_key(|declaration| index.declaration_header(*declaration).is_some())
    }

    /// Stable identity of one parser declaration in the active file-level declaration stream.
    /// Nested members are reached through their stable owner; this reverse map is only for choosing
    /// the bounded root that the parser handed to the checker.
    pub(crate) fn file_declaration(&self, file: &File, parser: DeclId) -> Option<DeclarationId> {
        self.declarations
            .iter()
            .enumerate()
            .find_map(|(raw, binding)| {
                let matches = match (*binding, file.decl(parser)) {
                    (Some(ActiveDeclarationRef::Function(candidate)), Decl::Fun(_)) => {
                        candidate == parser
                    }
                    (Some(ActiveDeclarationRef::Classifier(candidate)), Decl::Class(_)) => {
                        candidate == parser
                    }
                    (
                        Some(ActiveDeclarationRef::Property(ActivePropertyRef::TopLevel(
                            candidate,
                        ))),
                        Decl::Property(_),
                    ) => candidate == parser,
                    _ => false,
                };
                matches.then(|| {
                    DeclarationId::from_raw(
                        u32::try_from(raw)
                            .expect("active declaration table exceeds packed identity"),
                    )
                })
            })
    }

    pub(crate) fn top_level_property_declaration(&self, parser: DeclId) -> Option<DeclarationId> {
        self.declarations
            .iter()
            .enumerate()
            .find_map(|(raw, binding)| {
                (*binding
                    == Some(ActiveDeclarationRef::Property(ActivePropertyRef::TopLevel(
                        parser,
                    ))))
                .then(|| {
                    DeclarationId::from_raw(
                        u32::try_from(raw)
                            .expect("active declaration table exceeds packed identity"),
                    )
                })
            })
    }

    pub(crate) fn enum_entry_method_declaration(
        &self,
        class: DeclId,
        entry: u32,
        method: u32,
    ) -> Option<DeclarationId> {
        self.declarations
            .iter()
            .enumerate()
            .find_map(|(raw, binding)| {
                (*binding
                    == Some(ActiveDeclarationRef::EnumEntryMethod {
                        class,
                        entry,
                        index: method,
                    }))
                .then(|| {
                    DeclarationId::from_raw(
                        u32::try_from(raw)
                            .expect("active declaration table exceeds packed identity"),
                    )
                })
            })
    }

    pub(crate) fn source_member_declaration(
        &self,
        file: &File,
        index: &ResolvedModuleIndex,
        member: crate::libraries::SourceMember,
    ) -> Option<DeclarationId> {
        self.declarations
            .iter()
            .enumerate()
            .filter_map(|(raw, binding)| {
                let binding = (*binding)?;
                let matches = match (member, binding) {
                    (
                        crate::libraries::SourceMember::Class { owner, method, .. },
                        ActiveDeclarationRef::ClassMethod { class, index },
                    ) => class.0 == owner && index == method,
                    (
                        crate::libraries::SourceMember::EnumEntry {
                            owner,
                            entry,
                            method,
                            ..
                        },
                        ActiveDeclarationRef::EnumEntryMethod {
                            class,
                            entry: candidate_entry,
                            index,
                        },
                    ) => class.0 == owner && candidate_entry == entry && index == method,
                    (
                        crate::libraries::SourceMember::ClassProperty {
                            owner, property, ..
                        },
                        ActiveDeclarationRef::Property(ActivePropertyRef::ClassBody {
                            class,
                            index,
                        }),
                    ) => {
                        let primary = class_decl(file, class).map_or(0, |class| class.props.len());
                        class.0 == owner && primary + index as usize == property as usize
                    }
                    _ => false,
                };
                matches.then(|| {
                    DeclarationId::from_raw(
                        u32::try_from(raw)
                            .expect("active declaration table exceeds packed identity"),
                    )
                })
            })
            // Anonymous/local declarations can retain a parser-ancestry alias alongside the
            // repaired semantic declaration. Only the latter owns a published header and may
            // cross into checked FIR. This is the source-member counterpart of
            // `canonical_classifier_declaration`; both choose by stable semantic publication,
            // never by a source range or parser ID.
            .max_by_key(|declaration| index.declaration_header(*declaration).is_some())
    }

    pub(crate) fn function<'a>(
        &self,
        file: &'a File,
        declaration: DeclarationId,
    ) -> Option<&'a FunDecl> {
        match self.binding(declaration)? {
            ActiveDeclarationRef::Function(declaration) => match file.decl(declaration) {
                Decl::Fun(function) => Some(function),
                Decl::Class(_) | Decl::Property(_) => None,
            },
            ActiveDeclarationRef::ClassMethod { class, index } => {
                class_decl(file, class)?.methods.get(index as usize)
            }
            ActiveDeclarationRef::EnumEntryMethod {
                class,
                entry,
                index,
            } => class_decl(file, class)?
                .enum_entries
                .get(entry as usize)?
                .methods
                .get(index as usize),
            _ => None,
        }
    }

    pub(crate) fn property<'a>(
        &self,
        file: &'a File,
        declaration: DeclarationId,
    ) -> Option<&'a PropDecl> {
        let property = match self.binding(declaration)? {
            ActiveDeclarationRef::Property(property)
            | ActiveDeclarationRef::Accessor { property, .. } => property,
            _ => return None,
        };
        property_ref(file, property)
    }

    pub(crate) fn constructor_parameter<'a>(
        &self,
        file: &'a File,
        declaration: DeclarationId,
    ) -> Option<&'a crate::ast::PropParam> {
        let ActiveDeclarationRef::Property(ActivePropertyRef::ConstructorParameter {
            class,
            index,
        }) = self.binding(declaration)?
        else {
            return None;
        };
        class_decl(file, class)?.props.get(index as usize)
    }

    pub(crate) fn class<'a>(
        &self,
        file: &'a File,
        declaration: DeclarationId,
    ) -> Option<(DeclId, &'a ClassDecl)> {
        let ActiveDeclarationRef::Classifier(declaration) = self.binding(declaration)? else {
            return None;
        };
        Some((declaration, class_decl(file, declaration)?))
    }

    pub(crate) fn constructor<'a>(
        &self,
        file: &'a File,
        declaration: DeclarationId,
    ) -> Option<(DeclId, &'a ClassDecl, Option<&'a SecondaryCtor>)> {
        match self.binding(declaration)? {
            ActiveDeclarationRef::PrimaryConstructor { class } => {
                Some((class, class_decl(file, class)?, None))
            }
            ActiveDeclarationRef::SecondaryConstructor { class, index } => {
                let class_decl = class_decl(file, class)?;
                Some((
                    class,
                    class_decl,
                    class_decl.secondary_ctors.get(index as usize),
                ))
            }
            _ => None,
        }
    }

    pub(crate) fn enum_entry<'a>(
        &self,
        file: &'a File,
        declaration: DeclarationId,
    ) -> Option<&'a crate::ast::AstEnumEntry> {
        let ActiveDeclarationRef::EnumEntry { class, index } = self.binding(declaration)? else {
            return None;
        };
        class_decl(file, class)?.enum_entries.get(index as usize)
    }

    pub(crate) fn expression(
        &self,
        file: &File,
        declaration: DeclarationId,
    ) -> Option<crate::ast::ExprId> {
        match self.binding(declaration)? {
            ActiveDeclarationRef::ClassInitializer { class, index } => {
                match class_decl(file, class)?.init_order.get(index as usize)? {
                    ClassInit::Block(expression) => Some(*expression),
                    ClassInit::PropInit(_) => None,
                }
            }
            ActiveDeclarationRef::EnumEntryInitializer {
                class,
                entry,
                index,
            } => match class_decl(file, class)?
                .enum_entries
                .get(entry as usize)?
                .init_order
                .get(index as usize)?
            {
                ClassInit::Block(expression) => Some(*expression),
                ClassInit::PropInit(_) => None,
            },
            ActiveDeclarationRef::Script => file.script_body,
            _ => None,
        }
    }

    pub(crate) fn span(&self, file: &File, declaration: DeclarationId) -> Option<Span> {
        match self.binding(declaration)? {
            ActiveDeclarationRef::Function(_)
            | ActiveDeclarationRef::ClassMethod { .. }
            | ActiveDeclarationRef::EnumEntryMethod { .. } => {
                Some(self.function(file, declaration)?.span)
            }
            ActiveDeclarationRef::Property(_) => Some(self.property(file, declaration)?.span),
            ActiveDeclarationRef::Classifier(_) => Some(self.class(file, declaration)?.1.span),
            ActiveDeclarationRef::PrimaryConstructor { class } => {
                Some(class_decl(file, class)?.span)
            }
            ActiveDeclarationRef::SecondaryConstructor { class, index } => Some(
                class_decl(file, class)?
                    .secondary_ctors
                    .get(index as usize)?
                    .span,
            ),
            ActiveDeclarationRef::Accessor { property, setter } => {
                let property = property_ref(file, property)?;
                let body = if setter {
                    property.setter.as_ref()?.body.as_ref()
                } else {
                    property.getter.as_ref()
                };
                body.and_then(|body| fun_body_root(body).and_then(|root| file.expr_span(root)))
                    .or(Some(property.span))
            }
            ActiveDeclarationRef::ClassInitializer { .. }
            | ActiveDeclarationRef::EnumEntryInitializer { .. }
            | ActiveDeclarationRef::Script => file.expr_span(self.expression(file, declaration)?),
            ActiveDeclarationRef::EnumEntry { class, index } => Some(
                class_decl(file, class)?
                    .enum_entries
                    .get(index as usize)?
                    .span,
            ),
            ActiveDeclarationRef::TypeAlias | ActiveDeclarationRef::Generated => None,
        }
    }
}

/// Bind parser-only ancestry nodes (notably anonymous/local classifier owners) by their stable
/// owner-local structure. These declarations are absent from the public header inventory, but an
/// inventoried child can still name one as its owner. No source range participates in the match.
fn bind_auxiliary_parser_declarations(
    source: SourceFileId,
    index: &ResolvedModuleIndex,
    parser_ids: &DeclarationIds,
    parser_to_stable: &mut [Option<DeclarationId>],
) {
    loop {
        let mut changed = false;
        // An inventoried child gives an exact structural identity to a parser-only owner. Bind
        // that reverse edge before attempting owner-local matching: deeply nested anonymous
        // objects can have several headerless ancestry nodes with identical sibling ordinals, but
        // the selected child's stable owner is unambiguous.
        for raw in 0..parser_ids.len() {
            let Some(stable) = parser_to_stable.get(raw).and_then(|stable| *stable) else {
                continue;
            };
            let parser = DeclarationId::from_raw(
                u32::try_from(raw).expect("active parser declarations exceed packed identity"),
            );
            let Some(parser_owner) = parser_ids.anchor(parser).and_then(|anchor| anchor.owner)
            else {
                continue;
            };
            let Some(stable_owner) = index
                .declaration_anchor(stable)
                .and_then(|anchor| anchor.owner)
            else {
                continue;
            };
            let Some(slot) = parser_to_stable.get_mut(parser_owner.raw() as usize) else {
                continue;
            };
            if slot.is_none() {
                *slot = Some(stable_owner);
                changed = true;
            }
        }
        for raw in 0..parser_ids.len() {
            if parser_to_stable.get(raw).is_some_and(Option::is_some) {
                continue;
            }
            let parser = DeclarationId::from_raw(
                u32::try_from(raw).expect("active parser declarations exceed packed identity"),
            );
            let Some(anchor) = parser_ids.anchor(parser) else {
                continue;
            };
            let owner = match anchor.owner {
                Some(owner) => {
                    let Some(owner) = parser_to_stable
                        .get(owner.raw() as usize)
                        .and_then(|owner| *owner)
                    else {
                        continue;
                    };
                    Some(owner)
                }
                None => None,
            };
            let mut matches = (0..index.declaration_count()).filter_map(|stable_raw| {
                let stable = DeclarationId::from_raw(
                    u32::try_from(stable_raw).expect("stable declarations exceed packed identity"),
                );
                // A bounded parse renumbers `file.decls`, so a parser-only hoisted classifier can
                // accidentally have the same owner/kind/sibling tuple as an already-bound nested
                // classifier from the whole-file Pass-1 stream. Structural fallback is only for
                // identities absent from that direct stream; never steal one already claimed by a
                // real parser stub. Exact duplicate ancestry nodes are bound above through their
                // inventoried child's stable owner.
                if parser_to_stable
                    .iter()
                    .flatten()
                    .any(|bound| *bound == stable)
                {
                    return None;
                }
                // This fallback exists only for parser-ancestry nodes that have no semantic
                // declaration in the stable header stream. A declaration with a published header
                // belongs either to this bounded unit's direct zip (and is already bound) or to a
                // different unit. Matching that declaration structurally can otherwise bind a
                // top-level class from an earlier unit to a hoisted anonymous class in the current
                // one when both happen to be classifier sibling zero.
                if index.declaration_header(stable).is_some() {
                    return None;
                }
                index
                    .declaration_anchor(stable)
                    .filter(|candidate| {
                        candidate.source == source
                            && candidate.owner == owner
                            && candidate.kind == anchor.kind
                            && candidate.sibling == anchor.sibling
                    })
                    .map(|_| stable)
            });
            let Some(stable) = matches.next() else {
                continue;
            };
            if matches.next().is_some() {
                continue;
            }
            parser_to_stable[raw] = Some(stable);
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

fn active_binding(
    file: &File,
    kind: DeclarationKind,
    flags: DeclarationFlags,
    sibling: u32,
    owner: Option<ActiveDeclarationRef>,
) -> Option<ActiveDeclarationRef> {
    if flags.has(DeclarationFlags::COMPILER_GENERATED) {
        return Some(ActiveDeclarationRef::Generated);
    }
    if kind == DeclarationKind::Classifier {
        if flags.has(DeclarationFlags::COMPANION) {
            let ActiveDeclarationRef::Classifier(owner) = owner? else {
                return None;
            };
            return Some(ActiveDeclarationRef::Classifier(
                class_decl(file, owner)?.companion?,
            ));
        }
        let declaration = *file.decls.get(sibling as usize)?;
        return matches!(file.decl(declaration), Decl::Class(_))
            .then_some(ActiveDeclarationRef::Classifier(declaration));
    }
    match (kind, owner) {
        (DeclarationKind::Function, None) => {
            let declaration = *file.decls.get(sibling as usize)?;
            matches!(file.decl(declaration), Decl::Fun(_))
                .then_some(ActiveDeclarationRef::Function(declaration))
        }
        (DeclarationKind::Property, None) => {
            let declaration = *file.decls.get(sibling as usize)?;
            matches!(file.decl(declaration), Decl::Property(_)).then_some(
                ActiveDeclarationRef::Property(ActivePropertyRef::TopLevel(declaration)),
            )
        }
        (DeclarationKind::Constructor, Some(ActiveDeclarationRef::Classifier(class))) => {
            if sibling == 0 {
                Some(ActiveDeclarationRef::PrimaryConstructor { class })
            } else {
                Some(ActiveDeclarationRef::SecondaryConstructor {
                    class,
                    index: sibling - 1,
                })
            }
        }
        (DeclarationKind::Function, Some(ActiveDeclarationRef::Classifier(class))) => {
            Some(ActiveDeclarationRef::ClassMethod {
                class,
                index: sibling,
            })
        }
        (DeclarationKind::Property, Some(ActiveDeclarationRef::Classifier(class))) => Some(
            ActiveDeclarationRef::Property(if flags.has(DeclarationFlags::PROPERTY_PARAMETER) {
                ActivePropertyRef::ConstructorParameter {
                    class,
                    index: sibling,
                }
            } else {
                ActivePropertyRef::ClassBody {
                    class,
                    index: sibling,
                }
            }),
        ),
        (DeclarationKind::Initializer, Some(ActiveDeclarationRef::Classifier(class))) => {
            Some(ActiveDeclarationRef::ClassInitializer {
                class,
                index: sibling,
            })
        }
        (DeclarationKind::EnumEntry, Some(ActiveDeclarationRef::Classifier(class))) => {
            Some(ActiveDeclarationRef::EnumEntry {
                class,
                index: sibling,
            })
        }
        (
            DeclarationKind::Function,
            Some(ActiveDeclarationRef::EnumEntry {
                class,
                index: entry,
            }),
        ) => Some(ActiveDeclarationRef::EnumEntryMethod {
            class,
            entry,
            index: sibling,
        }),
        (
            DeclarationKind::Property,
            Some(ActiveDeclarationRef::EnumEntry {
                class,
                index: entry,
            }),
        ) => Some(ActiveDeclarationRef::Property(
            ActivePropertyRef::EnumEntry {
                class,
                entry,
                index: sibling,
            },
        )),
        (
            DeclarationKind::Initializer,
            Some(ActiveDeclarationRef::EnumEntry {
                class,
                index: entry,
            }),
        ) => Some(ActiveDeclarationRef::EnumEntryInitializer {
            class,
            entry,
            index: sibling,
        }),
        (DeclarationKind::Accessor, Some(ActiveDeclarationRef::Property(property))) => {
            Some(ActiveDeclarationRef::Accessor {
                property,
                setter: sibling == 1,
            })
        }
        (DeclarationKind::TypeAlias, _) => Some(ActiveDeclarationRef::TypeAlias),
        (DeclarationKind::Script, None) => Some(ActiveDeclarationRef::Script),
        _ => None,
    }
}

fn class_decl(file: &File, declaration: DeclId) -> Option<&ClassDecl> {
    match file.decl(declaration) {
        Decl::Class(class) => Some(class),
        Decl::Fun(_) | Decl::Property(_) => None,
    }
}

fn property_ref(file: &File, property: ActivePropertyRef) -> Option<&PropDecl> {
    match property {
        ActivePropertyRef::TopLevel(declaration) => match file.decl(declaration) {
            Decl::Property(property) => Some(property),
            Decl::Class(_) | Decl::Fun(_) => None,
        },
        ActivePropertyRef::ClassBody { class, index } => {
            class_decl(file, class)?.body_props.get(index as usize)
        }
        ActivePropertyRef::EnumEntry {
            class,
            entry,
            index,
        } => class_decl(file, class)?
            .enum_entries
            .get(entry as usize)?
            .props
            .get(index as usize),
        ActivePropertyRef::ConstructorParameter { .. } => None,
    }
}

fn fun_body_root(body: &FunBody) -> Option<crate::ast::ExprId> {
    match body {
        FunBody::Expr(root) | FunBody::Block(root) => Some(*root),
        FunBody::None => None,
    }
}
