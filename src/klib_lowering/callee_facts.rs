//! The provider records of the dependency declarations a checked call selected, by exact KLIB
//! identity.
//!
//! The body library's producer graph resolves a serialized call to an exact module-qualified
//! identity before this table is consulted. When a checked source call also selected that identity,
//! its provider record (normalized parameters, result and parameter identities, exactly as the
//! backend handoff froze them) must agree with the serialized header. A bare signature is never a
//! key and an unrelated source selection is never an origin fallback.

use std::rc::Rc;

use crate::fx_hash::{HashMap, HashSet};

use crate::libraries::{KlibBodyCallable, KlibDependencyCallable};

/// Frozen dependency declarations by their exact module-qualified KLIB identity.
#[derive(Clone, Default)]
pub struct KlibCalleeFacts<'a> {
    callables: HashMap<&'a KlibDependencyCallable, IndexedKlibCallee<'a>>,
    /// Builtin dispatch facts derived for each exact member identity before common-IR lowering.
    builtin_implementation_facts:
        Rc<HashMap<crate::fir::ExternalCallableId, Box<[crate::fir::BuiltinImplementation]>>>,
    /// Header evidence is not a checked source call which requires an executable export.
    selected: HashSet<&'a KlibDependencyCallable>,
    /// Same-compilation declaration identities for reference equality. These do not grant an
    /// executable external export or turn header evidence into a checked source selection.
    reference_targets: HashMap<&'a KlibDependencyCallable, crate::fir::ExternalCallableId>,
    /// Explicit same-compilation hierarchy evidence for authenticated dependency types.
    class_type_oracle: Option<&'a dyn crate::assignable::TypeOracle>,
    /// The classifiers whose representation the target owns, and so their singleton values too.
    target_classifiers: Option<&'a dyn Fn(crate::types::TypeName) -> bool>,
    /// The checked call edge facts of reachable dependency members, by declaration.
    ///
    /// This and the other frozen graph facts are filled once, before lowering derives per-body
    /// copies of the table with extra declarations, so the copies share them.
    member_edges: Rc<HashMap<KlibDependencyCallable, CallEdge>>,
    sam_declarations: Rc<HashMap<crate::ir::KlibSamKey, crate::ir::CheckedKlibSamDeclaration>>,
    /// The getter that bounds a counted loop over the indices of a collection or text, by the
    /// `indices` getter the closure reached. Resolved once, with the closure.
    indices_bounds: Rc<HashMap<KlibDependencyCallable, KlibDependencyCallable>>,
    /// The library objects and enum classes whose instances the closed bodies read, by classifier.
    read_classes: Rc<HashMap<crate::types::TypeName, crate::libraries::KlibBodyClassContract>>,
    /// The classes declaring a private member a class nested in them calls, by classifier.
    private_member_classes:
        Rc<HashMap<crate::types::TypeName, crate::libraries::KlibBodyClassContract>>,
    /// The checked edge of an enum's synthetic `entries` body: the selected `enumEntries`.
    enum_entries_factory: Option<KlibDependencyCallable>,
    /// The prepared-inline copies of the loaded libraries' inline functions, which a call of a
    /// suspend inline function expands from.
    expansion_copies: Option<&'a crate::klib_libraries::KlibDeclarationBodies>,
    /// The interface defaults a materialized object or local class declares and emits.
    object_defaults: Rc<HashSet<KlibDependencyCallable>>,
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

/// Shared name retained for the checked member-call edge consumed by KLIB body lowering.
pub(crate) type CallEdge = crate::ir::CheckedMemberCallEdge;

impl<'a> KlibCalleeFacts<'a> {
    /// Copy checker-produced semantic facts for the selected KLIB dependency graph.
    pub(crate) fn with_checked_dependency_facts(
        mut self,
        checked: &'a crate::ir::CheckedKlibDependencyFacts,
    ) -> Self {
        Rc::make_mut(&mut self.builtin_implementation_facts).extend(
            checked
                .builtin_implementations()
                .iter()
                .map(|(&id, facts)| (id, facts.clone())),
        );
        Rc::make_mut(&mut self.member_edges).extend(
            checked
                .member_edges()
                .iter()
                .map(|(id, edge)| (id.clone(), edge.clone())),
        );
        Rc::make_mut(&mut self.sam_declarations).extend(
            checked
                .sam_declarations()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        if let Some(hierarchy) = checked.type_hierarchy() {
            self.class_type_oracle = Some(hierarchy);
        }
        self
    }

    /// Record edges the closure authorized for members the checker has no function edge for.
    /// A checker edge is the hierarchy proof and is left as stored.
    pub(crate) fn with_authorized_member_edges(
        mut self,
        edges: impl IntoIterator<Item = (KlibDependencyCallable, CallEdge)>,
    ) -> Self {
        let member_edges = Rc::make_mut(&mut self.member_edges);
        for (identity, edge) in edges {
            member_edges.entry(identity).or_insert(edge);
        }
        self
    }

    /// Record the bounding getter the closure resolved for each `indices` getter it reached.
    pub(crate) fn with_indices_bounds(
        mut self,
        bounds: impl IntoIterator<Item = (KlibDependencyCallable, KlibDependencyCallable)>,
    ) -> Self {
        Rc::make_mut(&mut self.indices_bounds).extend(bounds);
        self
    }

    /// The getter that bounds a counted loop over the indices `getter` is.
    pub(super) fn indices_bound(
        &self,
        getter: &KlibDependencyCallable,
    ) -> Option<&KlibDependencyCallable> {
        self.indices_bounds.get(getter)
    }

    /// These facts with `factory`, the selected `enumEntries`: the checker freezing its edge, or a
    /// target that builds an enum's `entries` by calling it.
    pub(crate) fn with_enum_entries_factory(
        mut self,
        factory: Option<KlibDependencyCallable>,
    ) -> Self {
        self.enum_entries_factory = factory;
        self
    }

    /// The declaration an enum's synthetic `entries` body calls with the enum's `values()`.
    pub(super) fn enum_entries_factory(&self) -> Option<&KlibDependencyCallable> {
        self.enum_entries_factory.as_ref()
    }

    pub(super) fn sam_declaration(
        &self,
        module: &str,
        signature: &crate::metadata::id_signature::KlibPublicIdSignature,
    ) -> Option<&crate::ir::CheckedKlibSamDeclaration> {
        self.sam_declarations
            .get(&(module.to_owned(), signature.clone()))
    }

    pub(crate) fn builtin_implementation_facts(
        &self,
    ) -> &HashMap<crate::fir::ExternalCallableId, Box<[crate::fir::BuiltinImplementation]>> {
        &self.builtin_implementation_facts
    }

    /// Record the library objects and enum classes whose instances the closed bodies read.
    pub(crate) fn with_read_classes(
        mut self,
        objects: Vec<crate::libraries::KlibBodyClassContract>,
    ) -> Self {
        self.read_classes = Rc::new(
            objects
                .into_iter()
                .map(|object| (object.classifier, object))
                .collect(),
        );
        self
    }

    /// Record the classes declaring a private member a class nested in them calls.
    pub(crate) fn with_private_member_classes(
        mut self,
        classes: Vec<crate::libraries::KlibBodyClassContract>,
    ) -> Self {
        self.private_member_classes = Rc::new(
            classes
                .into_iter()
                .map(|class| (class.classifier, class))
                .collect(),
        );
        self
    }

    /// The class `classifier`, when a class nested in it calls a private member it declares.
    pub(crate) fn private_member_class(
        &self,
        classifier: crate::types::TypeName,
    ) -> Option<&crate::libraries::KlibBodyClassContract> {
        self.private_member_classes.get(&classifier)
    }

    pub(crate) fn with_object_defaults(
        mut self,
        defaults: impl IntoIterator<Item = KlibDependencyCallable>,
    ) -> Self {
        self.object_defaults = Rc::new(defaults.into_iter().collect());
        self
    }

    /// Whether the closure released `identity`, an interface default an object or a local class
    /// inherits.
    pub(crate) fn is_object_default(&self, identity: &KlibDependencyCallable) -> bool {
        self.object_defaults.contains(identity)
    }

    /// The object or enum class `classifier`, when the closed bodies read an instance of it.
    pub(crate) fn read_class(
        &self,
        classifier: crate::types::TypeName,
    ) -> Option<&crate::libraries::KlibBodyClassContract> {
        self.read_classes.get(&classifier)
    }

    /// The library objects and enum classes the closed bodies read, which the unit materializes.
    pub(crate) fn read_classes(
        &self,
    ) -> impl Iterator<Item = &crate::libraries::KlibBodyClassContract> {
        self.read_classes.values()
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

    /// The class hierarchy the checker froze for the selected bodies, when it froze one.
    pub(super) fn class_type_oracle(&self) -> Option<&'a dyn crate::assignable::TypeOracle> {
        self.class_type_oracle
    }

    #[cfg(test)]
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

    /// Expand a call of a suspend inline function from its prepared copy in `copies`.
    pub(crate) fn with_expansion_copies(
        mut self,
        copies: &'a crate::klib_libraries::KlibDeclarationBodies,
    ) -> Self {
        self.expansion_copies = Some(copies);
        self
    }

    /// The prepared copy of `declaration`, the ordinary serialized `identity`, that a call of it
    /// expands from: kotlinc links a call of a suspend inline function by inlining the copy its
    /// library prepared, whose suspension intrinsics are already lowered to the continuation
    /// operations. `None` for any other declaration.
    pub(super) fn expansion_copy(
        &self,
        identity: &KlibDependencyCallable,
        declaration: &crate::metadata::klib_ir::tree::KlibIrFunction,
    ) -> Option<(
        &'a crate::metadata::klib_ir::tree::KlibIrArena,
        &'a crate::metadata::klib_ir::tree::KlibIrFunction,
    )> {
        use crate::metadata::function_flags::{IS_INLINE, IS_SUSPEND};
        let flags = declaration.base.flags;
        if flags & IS_INLINE == 0 || flags & IS_SUSPEND == 0 {
            return None;
        }
        self.expansion_copies?.function(identity)
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

    pub(super) fn classifier_parameter_bounds(
        &self,
        owner: crate::types::TypeName,
        index: usize,
    ) -> Vec<crate::types::Ty> {
        super::type_evidence::classifier_parameter_bounds(self.class_type_oracle, owner, index)
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
        self.selected_constructor(identity)?
            .map(|selected| super::constructor_calls::authenticate(selected, bodies))
            .transpose()
    }

    fn selected_constructor(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<KlibBodyCallable<'a>>, super::decline::KlibBodyDeclineReason> {
        match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a constructor declaration".into(),
                ))
            }
            Some(KlibCalleeFact::Selected(callables))
                if callables[0].kind() == crate::libraries::ExternalCallableKind::Constructor =>
            {
                Ok(Some(callables[0]))
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

    /// An actual emitted initializer remains a declaration edge when its producer has no raw IR.
    /// Ambiguous provider buckets and every known malformed serialized header still decline.
    pub(crate) fn authenticates_retained_constructor(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<bool, super::decline::KlibBodyDeclineReason> {
        self.selected_constructor(identity)?
            .map(|selected| super::constructor_calls::authenticate_retained(selected, bodies))
            .transpose()
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

    /// The library objects the initialization of the class `identity` constructs reads as values.
    pub(crate) fn constructor_initialization_objects(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Vec<crate::metadata::id_signature::KlibPublicIdSignature>,
        super::decline::KlibBodyDeclineReason,
    > {
        super::class_initialization::constructor_initialization_objects(
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

    /// The initializer graph of `class`, an object or an enum class, when its initialization is
    /// admitted.
    pub(crate) fn declared_instances_callees(
        &self,
        class: &crate::libraries::KlibBodyClassContract,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Option<Vec<KlibDependencyCallable>>, super::decline::KlibBodyDeclineReason> {
        super::class_initialization::declared_instances_callees(class, bodies, self)
    }

    /// The library objects and enum classes the initialization of `class`, an object or an enum
    /// class, reads as values.
    pub(crate) fn declared_instances_objects(
        &self,
        class: &crate::libraries::KlibBodyClassContract,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Vec<crate::metadata::id_signature::KlibPublicIdSignature>,
        super::decline::KlibBodyDeclineReason,
    > {
        super::class_initialization::declared_instances_objects(class, bodies, self)
    }

    /// The members dispatch reaches of the object or enum class `class`.
    pub(crate) fn declared_instances_dispatched_members(
        &self,
        class: &crate::libraries::KlibBodyClassContract,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Vec<KlibDependencyCallable>, super::decline::KlibBodyDeclineReason> {
        super::class_initialization::declared_instances_dispatched_members(class, bodies, self)
    }

    /// The interface defaults the object `class` inherits, with what each implements.
    pub(crate) fn object_inherited_defaults(
        &self,
        class: &crate::libraries::KlibBodyClassContract,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Vec<super::class_initialization::InheritedDefault>,
        super::decline::KlibBodyDeclineReason,
    > {
        super::class_initialization::object_inherited_defaults(class, bodies, self)
    }

    /// The interface defaults the class `identity` constructs inherits, with what each implements.
    pub(crate) fn constructor_inherited_defaults(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<
        Vec<super::class_initialization::InheritedDefault>,
        super::decline::KlibBodyDeclineReason,
    > {
        match self.get(identity) {
            Some(KlibCalleeFact::Selected(callables)) => {
                super::class_initialization::constructor_inherited_defaults(
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

    /// The declarations the overrides of the object or enum class `class` override.
    pub(crate) fn declared_instances_overridden_declarations(
        &self,
        class: &crate::libraries::KlibBodyClassContract,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
    ) -> Result<Vec<KlibDependencyCallable>, super::decline::KlibBodyDeclineReason> {
        super::class_initialization::declared_instances_overridden_declarations(class, bodies, self)
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
        facts.add_declaration(declaration);
        facts
    }

    /// Record `declaration` in these facts, as [`Self::with_declaration`] does in a copy.
    pub(crate) fn add_declaration(&mut self, declaration: KlibBodyCallable<'a>) {
        self.record(declaration, false);
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

    /// The continuation intrinsic `identity` is, which body lowering realizes as the suspension
    /// point it stands for: no declaration of it is emitted or called.
    pub(crate) fn continuation_intrinsic(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<
        Option<super::continuation_intrinsics::ContinuationIntrinsicCall>,
        super::decline::KlibBodyDeclineReason,
    > {
        // Two selected declarations prove no intrinsic; the call's own linking reports them.
        match self.get(identity) {
            Some(KlibCalleeFact::Selected(callables)) => {
                super::continuation_intrinsics::authenticate(callables[0])
            }
            Some(KlibCalleeFact::Ambiguous) | None => Ok(None),
        }
    }

    /// The continuation interception `identity` is, which body lowering expands to the coroutine
    /// class's own interception: no declaration of it is emitted or called.
    pub(crate) fn intercepted_continuation(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<
        Option<super::intercepted_continuation::InterceptedContinuationCall>,
        super::decline::KlibBodyDeclineReason,
    > {
        // Two selected declarations prove no intrinsic; the call's own linking reports them.
        match self.get(identity) {
            Some(KlibCalleeFact::Selected(callables)) => {
                super::intercepted_continuation::authenticate(callables[0])
            }
            Some(KlibCalleeFact::Ambiguous) | None => Ok(None),
        }
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

    /// Authenticate the two target-owned builtin property reads whose serialized accessor is
    /// deliberately left outside the dependency-body unit. This keeps an exact `Array.size` or
    /// `String.length` target edge as its ordinary checked property read; other target intrinsics
    /// remain external calls and are not folded into common IR.
    pub(crate) fn target_property_read(
        &self,
        identity: &KlibDependencyCallable,
        bodies: &crate::klib_libraries::KlibDeclarationBodies,
        operation: crate::libraries::CompilerIntrinsic,
    ) -> Result<
        Option<super::primitive_calls::CommonPrimitiveCall>,
        super::decline::KlibBodyDeclineReason,
    > {
        use crate::libraries::CompilerIntrinsic::{ArraySize, StringLength};
        if !matches!(operation, ArraySize | StringLength) {
            return Ok(None);
        }
        let selected = match self.get(identity) {
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(super::decline::KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a target property getter".to_owned(),
                ));
            }
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            None => return Ok(None),
        };
        if selected.compiler_intrinsic() != Some(operation) {
            return Ok(None);
        }
        let Some(primitive) = super::primitive_calls::authenticate(selected, bodies)? else {
            return Ok(None);
        };
        if !matches!(
            primitive.operation,
            super::primitive_calls::CommonPrimitive::PropertyRead(found) if found == operation
        ) {
            return Ok(None);
        }
        Ok(Some(primitive))
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
            .and_modify(|fact| match fact {
                IndexedKlibCallee::Selected(selected)
                    if selected[0].same_declaration_contract(callable) =>
                {
                    if call_edge
                        && !selected.iter().any(|selected| {
                            selected.singleton_dispatch() == callable.singleton_dispatch()
                        })
                    {
                        selected.push(callable);
                    }
                }
                fact => *fact = IndexedKlibCallee::Ambiguous,
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
