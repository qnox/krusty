//! Stable body-unit ownership between signature finalization and checked FIR construction.

use super::body::ResolvedCallableHeader;
use super::header::{BodyKind, BodyOwnerId, DeclarationId, SourceMap, StreamedHeaderModule};
use super::signature::ResolvedModuleIndex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BodyWorkItem {
    pub declaration: DeclarationId,
    pub owner: BodyOwnerId,
    pub kind: BodyKind,
}

/// Semantic relationship between the declaration that owns live default-expression syntax and the
/// callable whose checked default FIR is retained.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DefaultArgumentRelation {
    /// The source declaration owns both the syntax and surviving callable signature.
    SameDeclaration,
    /// Expect syntax is checked against the structurally matched actual declaration and owner
    /// chain. The expect headers themselves do not survive signature finalization.
    ActualizedDeclaration,
    /// An overriding callable inherits syntax from a declaration in a different lexical owner.
    /// Only the callable identity is redirected; the provider's owner scope remains authoritative.
    InheritedOverride,
}

/// Pass-1 source/owner mapping for one callable's signature defaults. This mapping is consumed
/// while the provider's bounded syntax is live and never crosses Pass 1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DefaultArgumentProvider {
    pub target: DeclarationId,
    pub provider: DeclarationId,
    pub relation: DefaultArgumentRelation,
}

/// Pass-1-only executable inventory used to select and immediately check retained inline bodies.
/// Both this inventory and its same-pass checker-root map are destroyed before Pass 2 begins.
#[derive(Debug, Default)]
pub struct PassOneBodyInventory {
    units: Vec<BodyWorkItem>,
    /// Same-pass checker roots used only while preparing retained inline bodies. This map is
    /// destroyed before the Pass-1 result is returned.
    checker_roots: std::collections::HashMap<DeclarationId, DeclarationId>,
}

impl PassOneBodyInventory {
    pub(crate) fn units(&self) -> &[BodyWorkItem] {
        &self.units
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    pub fn storage_payload_bytes(&self) -> usize {
        self.units.len() * std::mem::size_of::<BodyWorkItem>()
    }

    /// Parser declaration ranges whose subtrees contain work in this queue. Nested classifiers are
    /// parser-hoisted declarations even though their stable semantic owner is another classifier,
    /// so a member stops at its nearest classifier rather than walking to the outermost owner. Enum
    /// entries are not parser declarations and continue to their enclosing enum classifier.
    pub(crate) fn top_level_ranges(
        &self,
        index: &ResolvedModuleIndex,
        source_count: usize,
    ) -> Vec<std::collections::HashSet<crate::diag::Span>> {
        let mut selected = vec![std::collections::HashSet::new(); source_count];
        for unit in &self.units {
            let root = self
                .checker_roots
                .get(&unit.declaration)
                .copied()
                .unwrap_or(unit.declaration);
            let anchor = index
                .declaration_anchor(root)
                .expect("every body unit root must retain its stable anchor");
            if let Some(ranges) = selected.get_mut(anchor.source.raw() as usize) {
                ranges.insert(
                    index
                        .declaration_range(root)
                        .expect("Pass-1 body roots retain same-pass coordinates"),
                );
            }
        }
        selected
    }

    /// Remove the next Pass-1 inline source's work as one source-ordered batch.
    pub fn take_for_source(
        &mut self,
        index: &ResolvedModuleIndex,
        source: super::SourceFileId,
    ) -> Vec<BodyWorkItem> {
        let split = self.units.partition_point(|unit| {
            index
                .declaration_anchor(unit.declaration)
                .is_some_and(|anchor| anchor.source < source)
        });
        assert_eq!(
            split, 0,
            "Pass 1 inline preparation must not skip an earlier source"
        );
        let count = self
            .units
            .iter()
            .take_while(|unit| {
                index
                    .declaration_anchor(unit.declaration)
                    .is_some_and(|anchor| anchor.source == source)
            })
            .count();
        self.units.drain(..count).collect()
    }

    /// Partition body units without inspecting syntax. The resolved callable index is the only
    /// authority for inline semantics. Both inventories are Pass-1 temporaries: the ordinary side
    /// is consulted only to include nested bodies owned by an inline root, then the whole partition
    /// is destroyed. Pass 2 rediscovers ordinary work from each active reparsed unit and receives no
    /// body queue from this object.
    pub fn partition_by_inline(self, index: &ResolvedModuleIndex) -> BodyPartition {
        let (inline, ordinary): (Vec<BodyWorkItem>, Vec<BodyWorkItem>) =
            self.units.into_iter().partition(|unit| {
                index
                    .callable_for_declaration(unit.declaration)
                    .is_some_and(ResolvedCallableHeader::is_inline)
            });
        BodyPartition {
            inline: PassOneBodyInventory {
                checker_roots: self
                    .checker_roots
                    .iter()
                    .filter(|(declaration, _)| {
                        inline.iter().any(|unit| unit.declaration == **declaration)
                    })
                    .map(|(declaration, root)| (*declaration, *root))
                    .collect(),
                units: inline,
            },
            ordinary: PassOneBodyInventory {
                checker_roots: self
                    .checker_roots
                    .iter()
                    .filter(|(declaration, _)| {
                        ordinary
                            .iter()
                            .any(|unit| unit.declaration == **declaration)
                    })
                    .map(|(declaration, root)| (*declaration, *root))
                    .collect(),
                units: ordinary,
            },
        }
    }
}

impl IntoIterator for PassOneBodyInventory {
    type Item = BodyWorkItem;
    type IntoIter = std::vec::IntoIter<BodyWorkItem>;

    fn into_iter(self) -> Self::IntoIter {
        self.units.into_iter()
    }
}

#[derive(Debug, Default)]
pub struct BodyPartition {
    pub inline: PassOneBodyInventory,
    pub ordinary: PassOneBodyInventory,
}

impl BodyPartition {
    /// Select every checker body nested under an inline callable. Local/anonymous declarations are
    /// separate stable body units even though their semantic lifetime belongs to the retained inline
    /// body; Pass 1 must therefore check their signatures before constructing that parent FIR.
    pub(crate) fn inline_check_selection(
        &self,
        index: &ResolvedModuleIndex,
        source_count: usize,
    ) -> BodyCheckSelection {
        let roots = self.inline.top_level_ranges(index, source_count);
        let inline_owners = self
            .inline
            .units
            .iter()
            .map(|unit| unit.declaration)
            .collect::<std::collections::HashSet<_>>();
        let mut bodies = vec![std::collections::HashSet::new(); source_count];
        let mut stable_bodies = vec![std::collections::HashSet::new(); source_count];
        let retained_inline_owner = |declaration: DeclarationId| {
            let mut current = Some(declaration);
            let mut seen = std::collections::HashSet::new();
            while let Some(candidate) = current {
                if !seen.insert(candidate) {
                    return None;
                }
                if inline_owners.contains(&candidate) {
                    return Some(candidate);
                }
                current = index
                    .declaration_header(candidate)
                    .and_then(|header| header.owner)
                    .or_else(|| {
                        index
                            .declaration_anchor(candidate)
                            .and_then(|anchor| anchor.owner)
                    });
            }
            None
        };
        let mut payload_roots = std::collections::HashMap::new();
        for unit in self.inline.units.iter().chain(&self.ordinary.units) {
            let root = self
                .inline
                .checker_roots
                .get(&unit.declaration)
                .or_else(|| self.ordinary.checker_roots.get(&unit.declaration))
                .copied()
                .unwrap_or(unit.declaration);
            let inline_owner =
                retained_inline_owner(root).or_else(|| retained_inline_owner(unit.declaration));
            let Some(inline_owner) = inline_owner else {
                continue;
            };
            let anchor = index
                .declaration_anchor(unit.declaration)
                .expect("every inline-owned body must retain its stable declaration anchor");
            let selected = &mut bodies[anchor.source.raw() as usize];
            selected.insert(
                index
                    .declaration_range(unit.declaration)
                    .expect("Pass-1 inline bodies retain same-pass coordinates"),
            );
            stable_bodies[anchor.source.raw() as usize].insert(unit.declaration);
            if !inline_owners.contains(&unit.declaration) {
                payload_roots.insert(unit.declaration, inline_owner);
            }
        }
        BodyCheckSelection {
            roots,
            bodies,
            stable_bodies,
            payload_roots,
        }
    }
}

pub(crate) struct BodyCheckSelection {
    pub roots: Vec<std::collections::HashSet<crate::diag::Span>>,
    pub bodies: Vec<std::collections::HashSet<crate::diag::Span>>,
    /// Stable identities whose syntax is checked as part of a retained inline subtree. This is
    /// transient Pass-1 selection state used to bind the live parser arena; it is consumed before
    /// ordinary body streaming and is never a source locator.
    pub stable_bodies: Vec<std::collections::HashSet<DeclarationId>>,
    /// Ordinary declaration bodies whose lexical lifetime is owned by a retained inline root.
    /// They are checked during Pass 1 and embedded into that root's FIR payload; this transient map
    /// is destroyed with the body inventory and never becomes a retained ordinary-body index.
    pub payload_roots: std::collections::HashMap<DeclarationId, DeclarationId>,
}

/// Stable parser-unit root for one declaration. Source containment is consulted only here, while
/// the initial parser/header structures are simultaneously live, to repair parser-hoisted local
/// classifier roots whose old AST model lacks an executable owner edge. Only the resulting stable
/// declaration identity leaves Pass 1 header preparation.
fn checker_root_for_declaration(
    index: &ResolvedModuleIndex,
    declarations: &super::DeclarationIds,
    local_classifier_lexical_roots: &std::collections::HashMap<DeclarationId, DeclarationId>,
    declaration: DeclarationId,
) -> DeclarationId {
    let ordinary_root = |mut declaration: DeclarationId| loop {
        let Some(anchor) = declarations.anchor(declaration) else {
            break declaration;
        };
        let local_classifier = anchor.kind == super::DeclarationKind::Classifier
            && index
                .declaration_header(declaration)
                .is_some_and(|header| header.flags.has(super::DeclarationFlags::LOCAL_CLASS));
        if (anchor.kind == super::DeclarationKind::Classifier && !local_classifier)
            || anchor.owner.is_none()
        {
            break declaration;
        }
        declaration = anchor.owner.expect("a non-root declaration has an owner");
    };

    let mut declaration = declaration;
    loop {
        let Some(anchor) = declarations.anchor(declaration) else {
            break declaration;
        };
        let local_classifier = anchor.kind == super::DeclarationKind::Classifier
            && index
                .declaration_header(declaration)
                .is_some_and(|header| header.flags.has(super::DeclarationFlags::LOCAL_CLASS));
        if local_classifier {
            if let Some(root) = local_classifier_lexical_roots
                .get(&declaration)
                .copied()
                .map(ordinary_root)
            {
                if root != declaration {
                    declaration = root;
                    continue;
                }
            }
        }
        if (anchor.kind == super::DeclarationKind::Classifier && !local_classifier)
            || anchor.owner.is_none()
        {
            break declaration;
        }
        declaration = anchor.owner.expect("a non-root declaration has an owner");
    }
}

impl StreamedHeaderModule {
    /// Publish the stable declaration stream as soon as signatures are finalized, without
    /// consuming the still-live compact syntax needed to check retained executable fragments.
    /// `finish` later observes this publication and only drops the original header-owned copy.
    pub(crate) fn publish_declaration_inventory(&self, index: &mut ResolvedModuleIndex) {
        assert!(
            !index.declarations_published(),
            "stable declarations may be published only once"
        );
        for raw in 0..self.sources.len() {
            let source = crate::fir::SourceFileId::from_raw(raw as u32);
            let package = self
                .sources
                .get(source)
                .expect("every stable source identity must retain its package")
                .package;
            index.publish_source_package(source, package);
        }
        index.publish_source_inventory(&self.inventory, &self.declarations);
        index.publish_declarations(self.declarations.clone());
    }

    /// Find the declaration whose live lexical traversal reaches one default provider. This query
    /// is used only while the initial Pass-1 parser/header state is simultaneously active; callers
    /// immediately check and detach the default FIR. The root is never stored in default work or
    /// carried across a phase boundary.
    pub(crate) fn lexical_root_for_default(
        &self,
        index: &ResolvedModuleIndex,
        provider: DeclarationId,
    ) -> DeclarationId {
        checker_root_for_declaration(
            index,
            &self.declarations,
            &self.local_classifier_lexical_roots,
            provider,
        )
    }

    /// Select inline body units while compact headers still own declaration ancestry. This is used
    /// before signature finalization only for inline capture preparation; ordinary bodies remain
    /// untouched until their active Pass-2 file.
    pub(crate) fn inline_body_ranges(&self, source_count: usize) -> BodyCheckSelection {
        let mut roots = vec![std::collections::HashSet::new(); source_count];
        let mut bodies = vec![std::collections::HashSet::new(); source_count];
        let mut stable_bodies = vec![std::collections::HashSet::new(); source_count];
        let inline_declarations = self
            .stubs
            .iter()
            .filter(|stub| stub.body.is_some() && stub.flags.has(super::DeclarationFlags::INLINE))
            .map(|stub| stub.id)
            .collect::<std::collections::HashSet<_>>();
        for stub in self.stubs.iter().filter(|stub| stub.body.is_some()) {
            let mut declaration = Some(stub.id);
            let mut retained_by_inline_owner = false;
            while let Some(candidate) = declaration {
                retained_by_inline_owner |= inline_declarations.contains(&candidate);
                declaration = self
                    .declarations
                    .anchor(candidate)
                    .and_then(|anchor| anchor.owner);
            }
            if !retained_by_inline_owner {
                continue;
            }
            if let Some(selected) = bodies.get_mut(stub.source.raw() as usize) {
                selected.insert(stub.range);
            }
            if let Some(selected) = stable_bodies.get_mut(stub.source.raw() as usize) {
                selected.insert(stub.id);
            }
            let mut declaration = stub.id;
            loop {
                let anchor = self
                    .declarations
                    .anchor(declaration)
                    .expect("every inline body must retain a stable declaration anchor");
                let Some(owner) = anchor.owner else {
                    if let Some(selected) = roots.get_mut(anchor.source.raw() as usize) {
                        selected.insert(anchor.range);
                    }
                    break;
                };
                declaration = owner;
            }
        }
        BodyCheckSelection {
            roots,
            bodies,
            stable_bodies,
            payload_roots: std::collections::HashMap::new(),
        }
    }

    /// Consume temporary Pass-1 header state after signature solving. The returned executable
    /// inventory exists only long enough to prepare retained inline FIR; ordinary syntax is later
    /// rediscovered directly from the sequential Pass-2 parser stream.
    pub fn finish(
        self,
        mut index: ResolvedModuleIndex,
    ) -> (ResolvedModuleIndex, SourceMap, PassOneBodyInventory) {
        let StreamedHeaderModule {
            sources,
            signature_origins: _,
            declarations,
            lookup_names,
            scopes,
            syntax,
            stubs,
            inventory,
            local_classifier_lexical_roots,
            inventoried: _,
            excluded: _,
            ..
        } = self;
        let mut units = stubs
            .iter()
            .filter_map(|stub| {
                stub.body.map(|kind| BodyWorkItem {
                    declaration: stub.id,
                    owner: stub.body_owner(),
                    kind,
                })
            })
            .collect::<Vec<_>>();
        let checker_roots = units
            .iter()
            .map(|unit| {
                (
                    unit.declaration,
                    checker_root_for_declaration(
                        &index,
                        &declarations,
                        &local_classifier_lexical_roots,
                        unit.declaration,
                    ),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        // Declaration inventory is grouped by declaration family, while executable initialization
        // semantics are source ordered. Establish that order before syntax disappears so a
        // consuming Pass-2 sink never has to retain/re-sort checked bodies.
        let source_order = inventory
            .iter()
            .copied()
            .enumerate()
            .map(|(order, declaration)| (declaration, order))
            .collect::<std::collections::HashMap<_, _>>();
        units.sort_by_key(|unit| {
            let anchor = declarations
                .anchor(unit.declaration)
                .expect("every body unit must retain its stable declaration anchor");
            (
                anchor.source,
                source_order
                    .get(&unit.declaration)
                    .copied()
                    .unwrap_or(usize::MAX),
                unit.declaration,
            )
        });
        drop((lookup_names, scopes, syntax));
        if !index.declarations_published() {
            index.publish_source_inventory(&inventory, &declarations);
            index.publish_declarations(declarations);
        }
        index.publish_local_classifier_lexical_roots(local_classifier_lexical_roots);
        (
            index,
            sources,
            PassOneBodyInventory {
                units,
                checker_roots,
            },
        )
    }
}
