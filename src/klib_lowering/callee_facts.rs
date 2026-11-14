//! The provider records of the dependency declarations a checked call selected, by exact KLIB
//! identity.
//!
//! The body library's producer graph resolves a serialized call to an exact module-qualified
//! identity before this table is consulted. When a checked source call also selected that identity,
//! its provider record (normalized parameters, result and parameter identities, exactly as the
//! backend handoff froze them) must agree with the serialized header. A bare signature is never a
//! key and an unrelated source selection is never an origin fallback.

use crate::fx_hash::{HashMap, HashSet};

use crate::libraries::{KlibBodyCallable, KlibDependencyCallable};

/// Frozen dependency declarations by their exact module-qualified KLIB identity.
#[derive(Clone, Default)]
pub struct KlibCalleeFacts<'a> {
    callables: HashMap<&'a KlibDependencyCallable, IndexedKlibCallee<'a>>,
    /// Header evidence is not a checked source call which requires an executable export.
    selected: HashSet<&'a KlibDependencyCallable>,
    /// Same-compilation declaration identities for reference equality. These do not grant an
    /// executable external export or turn header evidence into a checked source selection.
    reference_targets: HashMap<&'a KlibDependencyCallable, crate::fir::ExternalCallableId>,
    /// Explicit same-compilation hierarchy evidence for authenticated dependency types.
    class_type_oracle: Option<&'a dyn crate::assignable::TypeOracle>,
    /// The classifiers whose representation the target owns, and so their singleton values too.
    target_classifiers: Option<&'a dyn Fn(crate::types::TypeName) -> bool>,
    /// The call edge facts of members realized outside the unit, by declaration.
    member_edges: HashMap<KlibDependencyCallable, CallEdge>,
}

#[derive(Clone)]
enum IndexedKlibCallee<'a> {
    Selected(Vec<KlibBodyCallable<'a>>),
    /// Two frozen declarations claim the signature. Each identity names one declaration, so the
    /// table answers for neither rather than picking one.
    Ambiguous,
}

/// What one exact identity contributes to a lowering operation.
#[derive(Clone, Copy)]
pub(super) enum KlibCalleeFact<'facts, 'callable> {
    /// One declaration selected through one or more exact call-edge singleton variants.
    Selected(&'facts [KlibBodyCallable<'callable>]),
    Ambiguous,
}

/// The facts a call of a member realized outside a support unit carries beyond its callee, as the
/// member hierarchy decorates a source call of the same declaration: its semantic role, inherited
/// from a declaration it overrides when it has none of its own, and the declarations it
/// overrides, through which a target recognizes a member it implements specially.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CallEdge {
    pub(crate) semantic_role: Option<crate::types::SemanticCallRole>,
    pub(crate) overridden: Box<[crate::types::OverriddenDeclaration]>,
}

impl<'a> KlibCalleeFacts<'a> {
    /// The call edge facts of the members realized outside the unit, frozen with its closure.
    pub(crate) fn with_member_edges(
        mut self,
        edges: crate::fx_hash::HashMap<KlibDependencyCallable, CallEdge>,
    ) -> Self {
        self.member_edges = edges;
        self
    }

    pub(crate) fn member_edge(&self, identity: &KlibDependencyCallable) -> Option<&CallEdge> {
        self.member_edges.get(identity)
    }

    pub(crate) fn with_reference_declarations(
        selected: impl IntoIterator<Item = KlibBodyCallable<'a>>,
        declarations: impl IntoIterator<Item = (crate::fir::ExternalCallableId, KlibBodyCallable<'a>)>,
    ) -> Self {
        let mut facts = Self::new(selected);
        for (target, declaration) in declarations {
            facts.record(declaration, false);
            if facts
                .reference_targets
                .insert(declaration.identity(), target)
                .is_some_and(|previous| previous != target)
            {
                facts
                    .callables
                    .insert(declaration.identity(), IndexedKlibCallee::Ambiguous);
            }
        }
        facts
    }

    /// The same-compilation declaration `identity` names, when the unit's closure froze one.
    pub(super) fn reference_target(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<crate::fir::ExternalCallableId> {
        self.reference_targets.get(identity).copied()
    }

    pub(super) fn reference_declaration(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<
        (
            crate::fir::ExternalCallableId,
            crate::libraries::KlibBodyCallable<'a>,
        ),
        super::decline::KlibBodyDeclineReason,
    > {
        match self.get(identity) {
            Some(KlibCalleeFact::Selected(callables)) => self
                .reference_targets
                .get(identity)
                .copied()
                .map(|target| (target, callables[0]))
                .ok_or(super::decline::KlibBodyDeclineReason::UnsupportedOperation(
                    "a callable reference without an exact provider declaration",
                )),
            _ => Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                "a callable reference has no unique provider declaration".to_owned(),
            )),
        }
    }

    pub(crate) fn with_type_oracle(
        mut self,
        oracle: &'a dyn crate::assignable::TypeOracle,
    ) -> Self {
        self.class_type_oracle = Some(oracle);
        self
    }

    pub(crate) fn with_target_classifiers(
        mut self,
        represents: &'a dyn Fn(crate::types::TypeName) -> bool,
    ) -> Self {
        self.target_classifiers = Some(represents);
        self
    }

    /// Whether the target owns the representation of `classifier`, which a body then reads as the
    /// target's own value rather than through a class this unit materializes.
    pub(super) fn target_represents(&self, classifier: crate::types::TypeName) -> bool {
        self.target_classifiers
            .is_some_and(|represents| represents(classifier))
    }

    pub(super) fn constructor_bound(
        &self,
        parameters: &super::klib_types::KlibTypeParameters,
        argument: crate::types::Ty,
        bound: crate::types::Ty,
    ) -> bool {
        super::type_evidence::satisfies(self.class_type_oracle, parameters, argument, bound)
    }

    pub(super) fn is_subtype(
        &self,
        parameters: &super::klib_types::KlibTypeParameters,
        sub: crate::types::Ty,
        sup: crate::types::Ty,
    ) -> bool {
        super::type_evidence::ordinary_subtype(self.class_type_oracle, parameters, sub, sup)
    }

    pub(super) fn same_type(&self, left: crate::types::Ty, right: crate::types::Ty) -> bool {
        super::type_evidence::same_type(self.class_type_oracle, left, right)
    }

    /// Apply a receiver to the declaring classifier through the calling compilation's explicit
    /// class evidence. Without that evidence, only the receiver's own classifier is known.
    pub(super) fn applied_supertype(
        &self,
        receiver: crate::types::Ty,
        owner: crate::types::TypeName,
    ) -> Option<crate::types::Ty> {
        match self.class_type_oracle {
            Some(classes) => crate::assignable::applied_supertype(
                classes,
                receiver,
                crate::types::Ty::obj_name(owner),
            ),
            None => (receiver.kotlin_class_internal() == Some(owner)).then_some(receiver),
        }
    }

    pub(super) fn constructor(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Option<super::constructor_calls::ConstructorDeclaration>,
        super::decline::KlibBodyDeclineReason,
    > {
        match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a constructor declaration".to_owned(),
                ))
            }
            Some(KlibCalleeFact::Selected(callables))
                if callables[0].kind() == crate::libraries::ExternalCallableKind::Constructor =>
            {
                super::constructor_calls::authenticate(callables[0], bodies).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn authenticates_constructor(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<bool, super::decline::KlibBodyDeclineReason> {
        self.constructor(identity, bodies)
            .map(|constructor| constructor.is_some())
    }

    /// The initializer graph of the same positive ordinary-class capability used by lowering.
    pub(crate) fn constructor_initialization_callees(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Option<Vec<KlibDependencyCallable>>, super::decline::KlibBodyDeclineReason> {
        super::class_initialization::constructor_initialization_callees(
            self.initialized_class_member(identity)?,
            bodies,
            self,
        )
    }

    fn initialized_class_member(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<KlibBodyCallable<'a>, super::decline::KlibBodyDeclineReason> {
        match self.get(identity) {
            Some(KlibCalleeFact::Selected(callables)) => Ok(callables[0]),
            _ => Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                "an initializer has no unique selected constructor declaration".to_owned(),
            )),
        }
    }

    /// The members dispatch reaches of the class `identity` constructs.
    pub(crate) fn constructor_dispatched_members(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Vec<KlibDependencyCallable>, super::decline::KlibBodyDeclineReason> {
        match self.get(identity) {
            Some(KlibCalleeFact::Selected(callables)) => {
                super::class_initialization::constructor_dispatched_members(
                    callables[0],
                    bodies,
                    self,
                )
            }
            _ => Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                "an initializer has no unique selected constructor declaration".to_owned(),
            )),
        }
    }

    /// The declarations the initialization of the class `identity` constructs names without
    /// emitting their bodies.
    pub(crate) fn constructor_declaration_evidence(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Vec<KlibDependencyCallable>, super::decline::KlibBodyDeclineReason> {
        match self.get(identity) {
            Some(KlibCalleeFact::Selected(callables)) => {
                super::class_initialization::constructor_declaration_evidence(
                    callables[0],
                    bodies,
                    self,
                )
            }
            _ => Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                "an initializer has no unique selected constructor declaration".to_owned(),
            )),
        }
    }

    /// The initializer graph of the object `class`, when its initialization is admitted.
    pub(crate) fn object_initialization_callees(
        &self,
        class: &crate::libraries::KlibBodyClassContract,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Option<Vec<KlibDependencyCallable>>, super::decline::KlibBodyDeclineReason> {
        super::class_initialization::object_initialization_callees(class, bodies, self)
    }

    /// The members dispatch reaches of the object `class`.
    pub(crate) fn object_dispatched_members(
        &self,
        class: &crate::libraries::KlibBodyClassContract,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Vec<KlibDependencyCallable>, super::decline::KlibBodyDeclineReason> {
        super::class_initialization::object_dispatched_members(class, bodies, self)
    }

    /// The declarations the overrides of the object `class` override.
    pub(crate) fn object_overridden_declarations(
        &self,
        class: &crate::libraries::KlibBodyClassContract,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Vec<KlibDependencyCallable>, super::decline::KlibBodyDeclineReason> {
        super::class_initialization::object_overridden_declarations(class, bodies, self)
    }

    /// Check graph-owned declarations even when the root operation emits no callee body/export.
    pub(super) fn validate_root_constructor(
        &self,
        module: &str,
        signature: &crate::metadata::id_signature::KlibPublicIdSignature,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<(), super::decline::KlibBodyDeclineReason> {
        use super::decline::KlibBodyDeclineReason;
        use crate::klib_libraries::KlibCalleeOrigin;
        use crate::libraries::KlibDeclarationSignature;
        let mismatch = |detail: &str| KlibBodyDeclineReason::SignatureMismatch(detail.to_owned());
        let declaration = KlibDeclarationSignature::Public(signature.clone());
        let exact = match bodies.linked_origin(module, &declaration) {
            KlibCalleeOrigin::Ambiguous => {
                return Err(mismatch("ambiguous root constructor producer"))
            }
            KlibCalleeOrigin::Exact(identity) => Some(identity),
            KlibCalleeOrigin::Missing => None,
        };
        let reachable = bodies.body_provider_modules(module);
        let mut known = self
            .callables
            .keys()
            .copied()
            .filter(|identity| {
                identity.declaration == declaration
                    && reachable.contains(&identity.module)
                    && exact.as_ref().is_none_or(|exact| *identity == exact)
            })
            .collect::<Vec<_>>();
        if exact.is_none() && known.iter().any(|identity| identity.module == module) {
            known.retain(|identity| identity.module == module);
        }
        if known.len() > 1 {
            return Err(mismatch("ambiguous frozen root constructor producer"));
        }
        for identity in known {
            let Some(KlibCalleeFact::Selected(callables)) = self.get(identity) else {
                return Err(mismatch(
                    "conflicting provider contracts for the root constructor",
                ));
            };
            super::constructor_calls::authenticate_root_declaration(callables[0])?;
            if exact.is_some() {
                super::constructor_calls::authenticate(callables[0], bodies)?;
            }
        }
        if let Some(identity) = exact {
            super::constructor_calls::authenticate_root_header(&identity, bodies)?;
        }
        Ok(())
    }

    /// Authenticate a declaration the target implements without lowering its serialized body.
    pub(crate) fn target_intrinsic(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
        supports: &dyn Fn(crate::libraries::CompilerIntrinsic) -> bool,
    ) -> Result<Option<crate::libraries::CompilerIntrinsic>, super::decline::KlibBodyDeclineReason>
    {
        use super::decline::KlibBodyDeclineReason;
        use super::function_lowering::{selected_function_header, HeaderPurpose};
        let selected = match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a target declaration".to_owned(),
                ));
            }
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            None => return Ok(None),
        };
        let Some(operation) = selected
            .compiler_intrinsic()
            .filter(|operation| supports(*operation))
        else {
            return Ok(None);
        };
        let (arena, declaration) = bodies
            .function(identity)
            .ok_or(KlibBodyDeclineReason::Unjoined)?;
        // A body-bearing target leaf still authenticates its complete declared header, not the
        // narrower set of declarations whose executable bodies common lowering supports.
        selected_function_header(
            selected,
            HeaderPurpose::External,
            arena,
            declaration,
            Some(bodies),
        )?;
        Ok(Some(operation))
    }

    pub(crate) fn with_declaration(&self, declaration: KlibBodyCallable<'a>) -> Self {
        let mut facts = self.clone();
        facts.record(declaration, false);
        facts
    }

    pub(crate) fn target_getter(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
        supports: &dyn Fn(crate::libraries::DependencyGetterSignature<'_>) -> bool,
    ) -> Result<bool, super::decline::KlibBodyDeclineReason> {
        let selected = match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a getter declaration".to_owned(),
                ));
            }
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            None => return Ok(false),
        };
        if !super::accessor_calls::signature(selected).is_some_and(supports) {
            return Ok(false);
        }
        super::accessor_calls::authenticate(selected, bodies).map(|getter| getter.is_some())
    }

    /// Whether `identity` is a member of a classifier whose representation the target owns.
    pub(crate) fn target_member(
        &self,
        identity: &KlibDependencyCallable,
        represents: &dyn Fn(crate::types::TypeName) -> bool,
    ) -> Result<bool, super::decline::KlibBodyDeclineReason> {
        let selected = match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a member declaration".to_owned(),
                ));
            }
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            None => return Ok(false),
        };
        Ok(selected.is_member()
            && matches!(
                selected.declaration_owner(),
                Some(crate::types::SemanticCallableOwner::Classifier(owner)) if represents(owner)
            ))
    }

    pub(crate) fn common_primitive(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Option<super::primitive_calls::CommonPrimitiveCall>,
        super::decline::KlibBodyDeclineReason,
    > {
        // A target that does not lay out the range class builds it by calling the declaration,
        // as a source range construction on that target does.
        Ok(self.primitive(identity, bodies)?.filter(|primitive| {
            !matches!(
                primitive.operation,
                super::primitive_calls::CommonPrimitive::RangeConstruction(_)
            ) || primitive
                .ret
                .kotlin_class_internal()
                .is_some_and(|class| self.target_represents(class))
        }))
    }

    /// The range construction `identity` performs, whatever the target lays out. A membership in
    /// a range literal compares against its bounds, as kotlinc folds it, so it builds no range.
    pub(crate) fn range_literal_bounds(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Option<super::primitive_calls::CommonPrimitiveCall>,
        super::decline::KlibBodyDeclineReason,
    > {
        Ok(self.primitive(identity, bodies)?.filter(|primitive| {
            matches!(
                primitive.operation,
                super::primitive_calls::CommonPrimitive::RangeConstruction(_)
            )
        }))
    }

    fn primitive(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Option<super::primitive_calls::CommonPrimitiveCall>,
        super::decline::KlibBodyDeclineReason,
    > {
        match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a primitive declaration".to_owned(),
                ))
            }
            Some(KlibCalleeFact::Selected(callables)) if !self.is_selected(identity) => {
                super::primitive_calls::authenticate(callables[0], bodies)
            }
            _ => Ok(None),
        }
    }

    /// Index the frozen views of a caller's selected dependency declarations.
    pub fn new(callables: impl IntoIterator<Item = KlibBodyCallable<'a>>) -> Self {
        let mut facts = Self::default();
        for callable in callables {
            facts.selected.insert(callable.identity());
            facts.record(callable, true);
        }
        facts
    }

    /// Add exact provider header contracts without inventing source-call/export obligations.
    #[cfg(test)]
    pub(crate) fn with_declarations(
        selected: impl IntoIterator<Item = KlibBodyCallable<'a>>,
        declarations: impl IntoIterator<Item = KlibBodyCallable<'a>>,
    ) -> Self {
        let mut facts = Self::new(selected);
        for declaration in declarations {
            facts.record(declaration, false);
        }
        facts
    }

    pub(super) fn is_selected(&self, identity: &KlibDependencyCallable) -> bool {
        self.selected.contains(identity)
    }

    fn record(&mut self, callable: KlibBodyCallable<'a>, call_edge: bool) {
        self.callables
            .entry(callable.identity())
            .and_modify(|fact| {
                let conflicts = match fact {
                    IndexedKlibCallee::Selected(selected) => {
                        if !selected[0].same_declaration_contract(callable) {
                            true
                        } else if call_edge {
                            if let Some(index) = selected.iter().position(|existing| {
                                existing.singleton_dispatch() == callable.singleton_dispatch()
                            }) {
                                selected[index].class_contract() != callable.class_contract()
                            } else {
                                selected.push(callable);
                                false
                            }
                        } else {
                            false
                        }
                    }
                    IndexedKlibCallee::Ambiguous => false,
                };
                if conflicts {
                    *fact = IndexedKlibCallee::Ambiguous;
                }
            })
            .or_insert_with(|| IndexedKlibCallee::Selected(vec![callable]));
    }

    pub(super) fn get<'facts>(
        &'facts self,
        identity: &KlibDependencyCallable,
    ) -> Option<KlibCalleeFact<'facts, 'a>> {
        match self.callables.get(identity)? {
            IndexedKlibCallee::Selected(selected) => {
                Some(KlibCalleeFact::Selected(selected.as_slice()))
            }
            IndexedKlibCallee::Ambiguous => Some(KlibCalleeFact::Ambiguous),
        }
    }
}
