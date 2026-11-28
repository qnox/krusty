//! The common-IR unit that holds the lowered bodies of selected dependency declarations.
//!
//! Lowering one selected declaration also lowers every dependency declaration its body calls,
//! transitively, so the unit is closed over calls: each call names a function of the same unit.
//! A callee is joined to its serialized declaration by exact module-qualified identity, as a body
//! is, and declared from that declaration's header; a provider record of the same identity, when a
//! checked call selected one, is cross-checked against the header. A declaration is lowered once
//! per module-qualified signature, and a call cycle links back to the function it already declared.
//! If any reached body declines, the unit is left exactly as it was.

use crate::fx_hash::{HashMap, HashSet};
use std::collections::VecDeque;

use super::body_lowering::{
    constant_value, lower_parameter_defaults, publish_parameter_defaults, BodyLowering, CallLinker,
    ConstructorRole, LinkedAccessorCall, LinkedCallableReference, LinkedCallee, LinkedConstructor,
    LoweredFunction, StaticField,
};
use super::callee_facts::{KlibCalleeFact, KlibCalleeFacts};
use super::decline::{KlibBodyDecline, KlibBodyDeclineReason};
use super::function_lowering::{
    declare_function, function_header, raw_function_header, FunctionHeader, HeaderPurpose,
};
use super::ir_builtins::{BuiltinCallDisposition, IrBuiltinOperators};
use super::lowering_checkpoint::LoweringCheckpoint;
use crate::fir::ExternalCallableId;
use crate::ir::{Callee, FunId, IrFile};
use crate::klib_libraries::{
    KlibCalleeOrigin, KlibDeclarationBodies, KlibIntrinsicCallable, KlibIntrinsicFacts,
};
use crate::libraries::{KlibBodyCallable, KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrBody, KlibIrField, KlibIrFunction, KlibIrStatement,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol};
use crate::types::{Ty, TypeName};

mod class_construction;
pub(crate) use class_construction::superclass_constructor_ordinal;
mod constructor_linking;
mod enum_materialization;
mod fake_overrides;
mod file_private_functions;
mod host_imports;
pub(crate) use fake_overrides::emitted_body;
#[cfg(test)]
mod initializer_capability_tests;
mod inline_functions;
mod local_class;
mod private_members;
mod sam_placement;
mod static_properties;
mod super_calls;

pub(crate) use private_members::is_private_class_member;

/// An exact dependency edge linked outside this support unit. Body exports, target instruction
/// leaves and constructor calls have distinct realization contracts.
#[derive(Clone, Copy)]
pub(crate) struct EmittedDependencyCallable {
    target: ExternalCallableId,
    realization: DependencyExternalRealization,
    singleton_dispatch: Option<Ty>,
    singleton_classifier: Option<TypeName>,
}

#[derive(Clone, Copy)]
enum DependencyExternalRealization {
    EmittedBody,
    TargetIntrinsic(crate::libraries::CompilerIntrinsic),
    Constructor,
    AllocatingConstructor,
    ConstructorInitializer,
    PropertyGetter,
    TargetMember,
    ProviderLeaf,
}

impl EmittedDependencyCallable {
    pub(crate) fn new(
        target: ExternalCallableId,
        singleton_dispatch: Option<Ty>,
        singleton_classifier: Option<TypeName>,
    ) -> Self {
        Self {
            target,
            realization: DependencyExternalRealization::EmittedBody,
            singleton_dispatch,
            singleton_classifier,
        }
    }

    pub(crate) fn target(self) -> ExternalCallableId {
        self.target
    }

    pub(crate) fn target_realization(
        target: ExternalCallableId,
        operation: crate::libraries::CompilerIntrinsic,
    ) -> Self {
        Self {
            realization: DependencyExternalRealization::TargetIntrinsic(operation),
            ..Self::new(target, None, None)
        }
    }

    pub(crate) fn constructor(target: ExternalCallableId) -> Self {
        Self {
            realization: DependencyExternalRealization::Constructor,
            ..Self::new(target, None, None)
        }
    }

    /// Actual emitted initialization code; this capability does not permit allocation.
    pub(crate) fn constructor_initializer(target: ExternalCallableId) -> Self {
        Self {
            realization: DependencyExternalRealization::ConstructorInitializer,
            ..Self::new(target, None, None)
        }
    }

    pub(crate) fn constructor_initializer_target(self) -> Option<ExternalCallableId> {
        matches!(
            self.realization,
            DependencyExternalRealization::ConstructorInitializer
                | DependencyExternalRealization::AllocatingConstructor
        )
        .then_some(self.target)
    }

    pub(crate) fn allocating_constructor(target: ExternalCallableId) -> Self {
        Self {
            realization: DependencyExternalRealization::AllocatingConstructor,
            ..Self::new(target, None, None)
        }
    }

    pub(crate) fn property_getter(target: ExternalCallableId) -> Self {
        Self {
            realization: DependencyExternalRealization::PropertyGetter,
            ..Self::new(target, None, None)
        }
    }

    /// A member of a classifier whose representation the target owns: the target realizes the
    /// call at its dispatch receiver, so the KLIB body, written against the library's own class
    /// layout, is never lowered.
    pub(crate) fn target_member(target: ExternalCallableId) -> Self {
        Self {
            realization: DependencyExternalRealization::TargetMember,
            ..Self::new(target, None, None)
        }
    }

    pub(crate) fn provider_leaf(target: ExternalCallableId) -> Self {
        Self {
            realization: DependencyExternalRealization::ProviderLeaf,
            ..Self::new(target, None, None)
        }
    }

    fn realizes_accessor(self) -> bool {
        matches!(
            self.realization,
            DependencyExternalRealization::EmittedBody
                | DependencyExternalRealization::TargetIntrinsic(_)
                | DependencyExternalRealization::PropertyGetter
                | DependencyExternalRealization::TargetMember
                | DependencyExternalRealization::ProviderLeaf
        )
    }

    fn target_intrinsic(self) -> Option<crate::libraries::CompilerIntrinsic> {
        match self.realization {
            DependencyExternalRealization::TargetIntrinsic(operation) => Some(operation),
            _ => None,
        }
    }

    fn is_target_member(self) -> bool {
        matches!(
            self.realization,
            DependencyExternalRealization::TargetMember
        )
    }

    fn is_constructor(self) -> bool {
        matches!(
            self.realization,
            DependencyExternalRealization::Constructor
                | DependencyExternalRealization::AllocatingConstructor
        )
    }

    pub(crate) fn singleton_dispatch(self) -> Option<Ty> {
        self.singleton_dispatch
    }

    pub(crate) fn singleton_classifier(self) -> Option<TypeName> {
        self.singleton_classifier
    }
}

/// Lowered dependency bodies in one common-IR file, one function per declaration signature.
pub struct DependencyBodyUnit {
    ir: IrFile,
    functions: HashMap<KlibDependencyCallable, FunId>,
    classes: HashMap<KlibDependencyCallable, crate::ir::ClassId>,
    /// The initialization each materialized constructor of an ordinary class runs.
    constructors: HashMap<KlibDependencyCallable, MaterializedConstructor>,
    /// Classes an earlier unit of the program materialized and exports the constants or the
    /// instance of: this unit reads them through those exports instead of holding a second copy.
    realized_classes: HashSet<TypeName>,
    /// Whether the target lays out a class an earlier unit realized from that unit's record, so
    /// this unit may add final members to it ([`crate::ir::ClassOwnership::EarlierUnit`]).
    adds_members_to_realized_classes: bool,
    authenticated_singleton_dispatches: HashMap<KlibDependencyCallable, Ty>,
    authenticated_singleton_classifiers: HashMap<KlibDependencyCallable, TypeName>,
    /// Private members of value classes and file-private functions, which only this unit's bodies
    /// call, each with its own formals and their bounds: no provider records them.
    private_members: HashMap<KlibDependencyCallable, OwnFormals>,
    /// The dispatch receiver type of each class member this unit emits, its `this`.
    member_receivers: HashMap<KlibDependencyCallable, Ty>,
    /// File-local classes materialized for bodies of their own library, keyed by that library.
    local_classes: HashMap<
        super::same_library_class::LocalIdentity,
        super::same_library_class::MaterializedLocalClass,
    >,
    /// Member functions of those classes, keyed by the same library and their file-local signature.
    local_functions: HashMap<super::same_library_class::LocalIdentity, LocalFunction>,
    /// The static each top-level property's backing field is stored in, by that field.
    statics: HashMap<static_properties::StaticIdentity, u32>,
    /// The initialization of each library file whose top-level functions this unit emits and whose
    /// initialization runs code, by its library and serialized file.
    file_initializations: HashMap<static_properties::FileIdentity, FunId>,
    /// The functions that already run their file's initialization first, or need none.
    initialized_functions: HashSet<FunId>,
    /// Whether the target stores constant top-level initializers before any code of their file
    /// runs, rather than in its file's initialization order.
    presets_constant_statics: bool,
    builtins: IrBuiltinOperators,
}

/// A constructor of an actually materialized class: its class and secondary ordinal, or `None`
/// for its primary constructor. Allocation eligibility is a separate declaration capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MaterializedClassConstructor {
    pub(crate) class: crate::ir::ClassId,
    pub(crate) secondary: Option<usize>,
}

/// The private initialization function a materialized constructor runs, and the secondary
/// constructor it is of a class without a primary one.
#[derive(Clone, Copy)]
struct MaterializedConstructor {
    initializer: FunId,
    secondary: Option<usize>,
}

/// A member function of a file-local class and the class it belongs to.
struct LocalFunction {
    function: FunId,
    class: super::same_library_class::LocalIdentity,
    /// The function's own type parameters and their declared bounds.
    formals: Vec<(crate::types::TyParamName, Vec<Ty>)>,
}

impl Default for DependencyBodyUnit {
    fn default() -> Self {
        Self {
            ir: IrFile::default(),
            functions: HashMap::default(),
            classes: HashMap::default(),
            constructors: HashMap::default(),
            realized_classes: HashSet::default(),
            adds_members_to_realized_classes: false,
            authenticated_singleton_dispatches: HashMap::default(),
            authenticated_singleton_classifiers: HashMap::default(),
            private_members: HashMap::default(),
            member_receivers: HashMap::default(),
            local_classes: HashMap::default(),
            local_functions: HashMap::default(),
            statics: HashMap::default(),
            file_initializations: HashMap::default(),
            initialized_functions: HashSet::default(),
            presets_constant_statics: false,
            builtins: IrBuiltinOperators::new(),
        }
    }
}

/// Where a unit reads the declarations it lowers: their serialized declarations and bodies, and
/// the provider records of the callees a checked call selected.
#[derive(Clone, Copy)]
struct Sources<'a> {
    bodies: &'a KlibDeclarationBodies,
    callees: &'a KlibCalleeFacts<'a>,
    intrinsics: &'a KlibIntrinsicFacts,
    externals: &'a HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
    /// Declaration handles for override edges. This is the provider's complete registry, not the
    /// call closure in `externals`.
    declarations: &'a dyn crate::symbol_source::SymbolSource,
}

struct UnregisteredDeclarations;

impl crate::symbol_source::SymbolSource for UnregisteredDeclarations {}

fn unregistered_declarations() -> &'static dyn crate::symbol_source::SymbolSource {
    static SOURCE: UnregisteredDeclarations = UnregisteredDeclarations;
    &SOURCE
}

/// A declared function whose body is still to be lowered.
struct PendingBody<'a> {
    identity: KlibDependencyCallable,
    arena: &'a KlibIrArena,
    symbol: &'a KlibIrSymbol,
    statements: &'a [KlibIrStatement],
    header: FunctionHeader,
    function: FunId,
    initialization: Option<(crate::ir::ClassId, PendingInitialization<'a>)>,
    value_member: Option<MemberClass<'a>>,
    local_body: Option<super::same_library_class::LocalBody<'a>>,
    /// The static a top-level property's initialization stores, and its serialized initializer.
    static_initialization: Option<(StaticField, crate::metadata::klib_ir::tree::KlibIrExprId)>,
}

/// What a materialized constructor's private initialization function runs.
enum PendingInitialization<'a> {
    /// The admitted initialization of the class's own fields and initializers.
    Class(Box<super::class_initialization::ClassInitialization<'a, 'a>>),
    /// A secondary constructor's statements after its delegation to another constructor of the
    /// class, whose arguments the class's secondary constructor `secondary` passes.
    OwnDelegation {
        secondary: usize,
        arguments: Vec<Option<crate::metadata::klib_ir::tree::KlibIrExprId>>,
    },
    /// The arguments of each constant of a materialized enum class, in declaration order, which
    /// lower to the constants' own rather than to the function's body.
    EnumEntries(Vec<Vec<crate::metadata::klib_ir::tree::KlibIrExprId>>),
}

/// The declaration of a body this unit emits, and for a value class member, its class.
struct BodyHeader<'a> {
    header: FunctionHeader,
    value_member: Option<MemberClass<'a>>,
}

/// The class of a member this unit emits: the class it materializes, the fields the member's
/// body reads through its receiver, in declaration order, and the provider's authenticated class
/// contract. A value class has exactly one field, its value.
#[derive(Clone)]
pub(super) struct MemberClass<'a> {
    class: crate::ir::ClassId,
    /// The serialized declaration of `class`.
    pub(super) declaring: &'a KlibIrSymbol,
    pub(super) fields: Vec<&'a KlibIrField>,
    /// The class's properties with a backing field, each with that field.
    pub(super) properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
    pub(super) contract: &'a crate::libraries::KlibBodyClassContract,
    /// An inner class's outer instance, which its bodies read from the field after `fields`.
    pub(super) outer: Option<super::same_library_class::OuterInstance<'a>>,
}

impl DependencyBodyUnit {
    /// A positive capability for graph preparation, using the same classification as lowering.
    /// Provider header evidence is not a source selection. Ambiguity, selected roots (including
    /// recursive calls), and non-public declarations never authorize folding a declaration edge.
    pub(crate) fn folds_common_builtin(
        &self,
        identity: &KlibDependencyCallable,
        callees: &KlibCalleeFacts<'_>,
    ) -> bool {
        let KlibDeclarationSignature::Public(signature) = &identity.declaration else {
            return false;
        };
        matches!(
            self.builtins.call_disposition(signature, || {
                if matches!(callees.get(identity), Some(KlibCalleeFact::Ambiguous)) {
                    return Err(());
                }
                Ok(callees.is_selected(identity))
            }),
            Ok(BuiltinCallDisposition::Common(_))
        )
    }

    /// Whether this selected declaration has the exact scalar boundary of an instruction wrapper:
    /// a non-member whose operands are scalars of an instruction class and whose result is such a
    /// scalar or `Boolean`. This is a semantic type/receiver contract, not a declaration-name
    /// allowlist. It prevents an arbitrary stdlib caller which happens to reach an instruction (for
    /// example a collection hash operation) from becoming executable support code.
    pub(crate) fn is_scalar_root_candidate(
        callable: KlibBodyCallable<'_>,
        bodies: &KlibDeclarationBodies,
    ) -> bool {
        if callable.is_member() {
            return false;
        }
        let Some((arena, declaration)) = bodies.function(callable.identity()) else {
            return false;
        };
        let Ok(header) = function_header(Some(callable), arena, declaration) else {
            return false;
        };
        let (params, ret, context_count, extension_receiver, dispatch_receiver) =
            header.linked_signature();
        if dispatch_receiver.is_some() {
            return false;
        }
        let mut operands = params.to_vec();
        if extension_receiver && header.singleton_extension_unread_receiver().is_some() {
            if context_count >= operands.len() {
                return false;
            }
            operands.remove(context_count);
        }
        !operands.is_empty()
            && operands
                .iter()
                .all(|operand| crate::libraries::ScalarClass::of(*operand).is_some())
            && (ret == Ty::Boolean || crate::libraries::ScalarClass::of(ret).is_some())
    }

    /// The function holding `callable`'s body, lowering it, and every body it reaches through
    /// calls, the first time its module-qualified identity is seen. A declined body leaves the
    /// unit as it was.
    ///
    /// `callees` holds the provider records of the callees a checked call selected; a callee it
    /// does not hold is described by its serialized header alone.
    #[cfg(test)]
    pub(crate) fn lower_function<'a>(
        &mut self,
        callable: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
    ) -> Result<FunId, KlibBodyDecline> {
        self.lower_function_with_intrinsics(callable, bodies, callees, KlibIntrinsicFacts::empty())
    }

    /// Lower with target-stdlib intrinsic declarations authenticated by the KLIB provider. A
    /// production caller supplies the table explicitly so an authenticated intrinsic declaration
    /// can never silently fall through to ordinary body lookup.
    pub fn lower_function_with_intrinsics<'a>(
        &mut self,
        callable: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
        intrinsics: &'a KlibIntrinsicFacts,
    ) -> Result<FunId, KlibBodyDecline> {
        static EMPTY: std::sync::OnceLock<
            HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
        > = std::sync::OnceLock::new();
        self.lower_function_with_intrinsics_and_externals(
            callable,
            bodies,
            callees,
            intrinsics,
            EMPTY.get_or_init(HashMap::default),
        )
    }

    /// Lower while linking a callee already emitted by an earlier streaming support unit through
    /// its provider identity. The exact KLIB identity remains the join key; the opaque external
    /// identity is only the checked IR handle for the ordinary backend dependency registry.
    pub(crate) fn lower_function_with_intrinsics_and_externals<'a>(
        &mut self,
        callable: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
        intrinsics: &'a KlibIntrinsicFacts,
        externals: &'a HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
    ) -> Result<FunId, KlibBodyDecline> {
        self.lower_function_with_declarations(
            callable,
            bodies,
            callees,
            intrinsics,
            externals,
            unregistered_declarations(),
        )
    }

    /// Lower one function, resolving override declaration handles from `declarations` even when
    /// the body never calls those members.
    pub(crate) fn lower_function_with_declarations<'a>(
        &mut self,
        callable: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
        intrinsics: &'a KlibIntrinsicFacts,
        externals: &'a HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
        declarations: &'a dyn crate::symbol_source::SymbolSource,
    ) -> Result<FunId, KlibBodyDecline> {
        if let Some(function) = self.functions.get(callable.identity()).copied() {
            // A member installed with its class was authenticated with the class, as was the
            // box construction of a value class materialized with its class.
            let value_construction = callable.kind()
                == crate::libraries::ExternalCallableKind::Constructor
                && callable
                    .class_contract()
                    .is_some_and(|class| class.is_value);
            if self.installs_member(callable.identity()) || value_construction {
                return Ok(function);
            }
            // Declared before, perhaps from its header alone as another body's callee: the
            // selection it is now asked for must describe it too.
            let (arena, declaration) = bodies.function(callable.identity()).ok_or_else(|| {
                KlibBodyDecline::new(
                    callable.signature().clone(),
                    KlibBodyDeclineReason::Unjoined,
                )
            })?;
            body_header(Some(callable), arena, declaration, bodies)
                .map_err(|reason| KlibBodyDecline::new(callable.signature().clone(), reason))?;
            self.record_authenticated_singleton(callable);
            return Ok(function);
        }
        let checkpoint = UnitCheckpoint::of(self);
        let lowered = self.lower_reachable(
            callable,
            Sources {
                bodies,
                callees,
                intrinsics,
                externals,
                declarations,
            },
        );
        lowered.map_err(|decline| {
            checkpoint.restore(self);
            if decline.declaration() == callable.signature() {
                decline
            } else {
                KlibBodyDecline::new(
                    callable.signature().clone(),
                    KlibBodyDeclineReason::CalleeDeclined(Box::new(decline)),
                )
            }
        })
    }

    /// Authenticate the complete checked callee-fact bucket for one root against its serialized
    /// header and lower the unique matching variant. Input order cannot decide target emission.
    ///
    /// Tests that do not install interface overrides use this. Production passes the provider.
    #[cfg(test)]
    pub(crate) fn lower_root_with_intrinsics_and_externals<'a>(
        &mut self,
        root: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
        intrinsics: &'a KlibIntrinsicFacts,
        externals: &'a HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
    ) -> Result<FunId, KlibBodyDecline> {
        self.lower_root_with_declarations(
            root,
            bodies,
            callees,
            intrinsics,
            externals,
            unregistered_declarations(),
        )
    }

    /// Lower one selected root, publishing override handles from `declarations`.
    pub(crate) fn lower_root_with_declarations<'a>(
        &mut self,
        root: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
        intrinsics: &'a KlibIntrinsicFacts,
        externals: &'a HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
        declarations: &'a dyn crate::symbol_source::SymbolSource,
    ) -> Result<FunId, KlibBodyDecline> {
        let callables = match callees.get(root.identity()) {
            Some(KlibCalleeFact::Selected(callables)) => callables,
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(KlibBodyDecline::new(
                    root.signature().clone(),
                    KlibBodyDeclineReason::SignatureMismatch(
                        "checked root variants disagree on their declaration contract".to_owned(),
                    ),
                ))
            }
            None => {
                return Err(KlibBodyDecline::new(
                    root.signature().clone(),
                    KlibBodyDeclineReason::SignatureMismatch(
                        "a selected KLIB root has no checked callee-fact bucket".to_owned(),
                    ),
                ))
            }
        };
        if root.kind() == crate::libraries::ExternalCallableKind::Constructor {
            callees
                .constructor(root.identity(), bodies)
                .map_err(|reason| KlibBodyDecline::new(root.signature().clone(), reason))?
                .ok_or_else(|| {
                    KlibBodyDecline::new(
                        root.signature().clone(),
                        KlibBodyDeclineReason::SignatureMismatch(
                            "a constructor root lost its selected declaration".to_owned(),
                        ),
                    )
                })?;
            return self.lower_function_with_declarations(
                callables[0],
                bodies,
                callees,
                intrinsics,
                externals,
                declarations,
            );
        }
        if root.kind() == crate::libraries::ExternalCallableKind::Member
            && callables
                .iter()
                .all(|callable| callable.singleton_dispatch().is_none())
            && root
                .class_contract()
                .is_some_and(|class| !class.is_value && !class.is_builtin_type())
        {
            // A member of an ordinary class is authenticated with the class that installs it.
            // Singleton edges of one declaration differ in the class they dispatch to; the
            // receiver proof selects among them.
            let [callable] = callables else {
                return Err(KlibBodyDecline::new(
                    root.signature().clone(),
                    KlibBodyDeclineReason::SignatureMismatch(
                        "a class member root has several selected declarations".to_owned(),
                    ),
                ));
            };
            return self.lower_function_with_declarations(
                *callable,
                bodies,
                callees,
                intrinsics,
                externals,
                declarations,
            );
        }
        let (arena, declaration) = bodies.function(root.identity()).ok_or_else(|| {
            KlibBodyDecline::new(root.signature().clone(), KlibBodyDeclineReason::Unjoined)
        })?;
        let callable = selected_callable_for_header_purpose(
            Some(SelectedKlibCallables::Variants(callables)),
            HeaderPurpose::Body(None),
            arena,
            declaration,
            Some(bodies),
        )
        .map_err(|reason| KlibBodyDecline::new(root.signature().clone(), reason))?
        .expect("a selected root bucket contains a callable");
        self.lower_function_with_declarations(
            callable,
            bodies,
            callees,
            intrinsics,
            externals,
            declarations,
        )
    }

    pub fn ir(&self) -> &IrFile {
        &self.ir
    }

    /// Carry the selected declarations' builtin implementations into this checked body unit.
    pub(crate) fn record_builtin_implementation_facts(
        &mut self,
        callees: &KlibCalleeFacts<'_>,
    ) -> Result<(), crate::fir::ExternalCallableId> {
        self.ir
            .record_external_builtin_implementations(callees.builtin_implementation_facts())
    }

    /// Singleton receiver of the exact call-edge variant which authenticated this emitted body.
    pub(crate) fn authenticated_singleton_dispatch(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<Ty> {
        self.authenticated_singleton_dispatches
            .get(identity)
            .copied()
    }

    pub(crate) fn authenticated_singleton_classifier(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<TypeName> {
        self.authenticated_singleton_classifiers
            .get(identity)
            .copied()
    }

    fn record_authenticated_singleton(&mut self, callable: KlibBodyCallable<'_>) {
        if let Some(singleton) = callable.singleton_dispatch() {
            self.authenticated_singleton_dispatches
                .insert(callable.identity().clone(), singleton);
        }
        if let Some(classifier) = callable.singleton_classifier() {
            self.authenticated_singleton_classifiers
                .insert(callable.identity().clone(), classifier);
        }
    }

    /// Consume the closed dependency-body unit after its roots and all reachable callees have
    /// lowered successfully.
    pub fn into_ir(self) -> IrFile {
        self.ir
    }

    /// Whether `identity` is a member of a class this unit installed with all of its members.
    pub(super) fn installs_member(&self, identity: &KlibDependencyCallable) -> bool {
        self.local_functions
            .contains_key(&local_class::installed_identity(identity))
    }

    /// The receiver type of the class member `identity` this unit emits, which a call passes as
    /// the member's dispatch receiver.
    pub(crate) fn member_receiver(&self, identity: &KlibDependencyCallable) -> Option<Ty> {
        self.member_receivers.get(identity).copied()
    }

    /// Every exact dependency identity realized by this closed unit that a caller outside it can
    /// name, including functions reached transitively from its selected root. A private member
    /// of a value class is called only by its class's members in this unit.
    pub fn callable_functions(
        &self,
    ) -> impl Iterator<Item = (&KlibDependencyCallable, FunId)> + '_ {
        self.functions
            .iter()
            .filter(|(identity, function)| {
                !self.private_members.contains_key(*identity)
                    && !self.ir.inline_only_fns.contains(*function)
            })
            .map(|(identity, &function)| (identity, function))
    }

    /// Actual constructor initializers materialized in this unit, including abstract classes.
    pub(crate) fn initializing_constructors(
        &self,
    ) -> impl Iterator<Item = (&KlibDependencyCallable, MaterializedClassConstructor)> {
        self.constructors.iter().map(|(identity, constructor)| {
            (
                identity,
                MaterializedClassConstructor {
                    class: self.classes[identity],
                    secondary: constructor.secondary,
                },
            )
        })
    }

    /// Allocation exports are separate from initializer declarations. Abstract classes have none.
    pub(crate) fn allocating_constructors(
        &self,
    ) -> impl Iterator<Item = (&KlibDependencyCallable, MaterializedClassConstructor)> {
        self.initializing_constructors().filter(|(_, constructor)| {
            let class = &self.ir.classes[constructor.class as usize];
            !class.is_abstract && !class.is_sealed && !class.is_object && !class.is_enum
        })
    }

    /// A unit that reads the enum constants and object instances of `classes`, which earlier
    /// units materialized, through their exports.
    pub(crate) fn reading_realized_classes(
        classes: impl IntoIterator<Item = TypeName>,
        reference: crate::kotlin_version::ReferenceProfile,
    ) -> Self {
        let mut unit = Self {
            realized_classes: classes.into_iter().collect(),
            ..Self::default()
        };
        unit.ir.reference = reference;
        unit
    }

    /// This unit for a target that lays out a class an earlier unit realized from that unit's
    /// record, so a member this unit needs of such a class is added to it here
    /// ([`crate::backend::Backend::adds_members_to_realized_classes`]).
    pub(crate) fn adding_members_to_realized_classes(mut self, adds: bool) -> Self {
        self.adds_members_to_realized_classes = adds;
        self
    }

    /// Whether an earlier unit materialized `classifier`, which this unit reads through its
    /// exports.
    pub(crate) fn reads_realized(&self, classifier: TypeName) -> bool {
        self.realized_classes.contains(&classifier)
    }

    /// This unit for a target that stores constant top-level initializers before any code of
    /// their file runs (`presets`), or one that runs them in their file's initialization order.
    pub(crate) fn presetting_constant_statics(mut self, presets: bool) -> Self {
        self.presets_constant_statics = presets;
        self
    }

    /// The library and backing field of each top-level property this unit stores.
    pub(crate) fn materialized_statics(&self) -> impl Iterator<Item = (&str, &KlibIrSymbol)> + '_ {
        self.statics
            .keys()
            .map(|(module, field)| (module.as_str(), field))
    }

    /// Classes owned by this closed unit, keyed by their authenticated constructor.
    pub(crate) fn materialized_classes(
        &self,
    ) -> impl Iterator<Item = (TypeName, &KlibDependencyCallable)> {
        let mut classes = self.classes.iter().collect::<Vec<_>>();
        classes.sort_unstable_by_key(|(_, class)| **class);
        classes
            .into_iter()
            .map(|(identity, class)| (self.ir.classes[*class as usize].fq_name, identity))
    }

    fn lower_reachable<'a>(
        &mut self,
        callable: KlibBodyCallable<'a>,
        sources: Sources<'a>,
    ) -> Result<FunId, KlibBodyDecline> {
        self.lower_seeded(
            Some(callable),
            callable.identity(),
            sources,
            |linker, ir| {
                let (arena, function) = linker
                    .sources
                    .bodies
                    .function(callable.identity())
                    .ok_or(KlibBodyDeclineReason::Unjoined)?;
                if callable.kind() == crate::libraries::ExternalCallableKind::Constructor
                    && callable
                        .class_contract()
                        .is_some_and(|class| class.is_value)
                {
                    linker.materialize_value_constructor(ir, callable)
                } else if callable.kind() == crate::libraries::ExternalCallableKind::Constructor {
                    linker.materialize_class(ir, callable.identity(), callable)?;
                    let constructor = linker.constructors.get(callable.identity()).ok_or(
                        KlibBodyDeclineReason::UnsupportedDeclaration(
                            "a constructor without ordinary initialization",
                        ),
                    )?;
                    Ok(constructor.initializer)
                } else {
                    linker.declare(ir, callable.identity(), Some(callable), arena, function)
                }
            },
        )
    }

    /// Materialize the library object `class`, which the emitted source reads as a value, with
    /// its instance and every body its initialization and dispatched members reach.
    pub(crate) fn lower_object<'a>(
        &mut self,
        class: &'a crate::libraries::KlibBodyClassContract,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
        intrinsics: &'a KlibIntrinsicFacts,
        externals: &'a HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
        declarations: &'a dyn crate::symbol_source::SymbolSource,
    ) -> Result<(), KlibBodyDecline> {
        let primary = class.primary_constructor.clone().ok_or_else(|| {
            KlibBodyDecline::new(
                KlibDeclarationSignature::Public(class.signature.clone()),
                KlibBodyDeclineReason::UnsupportedOperation("an object value"),
            )
        })?;
        let identity = KlibDependencyCallable {
            module: class.module.clone(),
            declaration: KlibDeclarationSignature::Public(primary),
        };
        if self.classes.contains_key(&identity) {
            return Ok(());
        }
        let checkpoint = UnitCheckpoint::of(self);
        let lowered = self.lower_seeded(
            None,
            &identity,
            Sources {
                bodies,
                callees,
                intrinsics,
                externals,
                declarations,
            },
            |linker, ir| {
                linker.materialize_declared_instances(ir, &identity, class)?;
                if linker.classes.contains_key(&identity) {
                    Ok(())
                } else {
                    Err(KlibBodyDeclineReason::UnsupportedOperation(
                        "an object value",
                    ))
                }
            },
        );
        lowered.inspect_err(|_| checkpoint.restore(self))
    }

    /// Lower every body reached from what `seed` declares in a linker whose root selection is
    /// `root`, the bodies of `identity`'s module first.
    fn lower_seeded<'a, T>(
        &mut self,
        root: Option<KlibBodyCallable<'a>>,
        identity: &KlibDependencyCallable,
        sources: Sources<'a>,
        seed: impl FnOnce(&mut Linker<'_, 'a>, &mut IrFile) -> Result<T, KlibBodyDeclineReason>,
    ) -> Result<T, KlibBodyDecline> {
        let (first_class, classes) = (self.ir.classes.len(), sources.callees.class_type_oracle());
        let mut pending = VecDeque::new();
        let mut linker = Linker {
            functions: &mut self.functions,
            classes: &mut self.classes,
            constructors: &mut self.constructors,
            realized_classes: &self.realized_classes,
            adds_members_to_realized_classes: self.adds_members_to_realized_classes,
            authenticated_singleton_dispatches: &mut self.authenticated_singleton_dispatches,
            authenticated_singleton_classifiers: &mut self.authenticated_singleton_classifiers,
            private_members: &mut self.private_members,
            member_receivers: &mut self.member_receivers,
            local_classes: &mut self.local_classes,
            local_functions: &mut self.local_functions,
            statics: &mut self.statics,
            file_initializations: &mut self.file_initializations,
            initialized_functions: &mut self.initialized_functions,
            presets_constant_statics: self.presets_constant_statics,
            pending: &mut pending,
            root,
            active_module: identity.module.clone(),
            active_identity: identity.clone(),
            active_value_class: None,
            member_classes: Vec::new(),
            sources,
        };
        let root = seed(&mut linker, &mut self.ir)
            .map_err(|reason| KlibBodyDecline::new(identity.declaration.clone(), reason))?;
        loop {
            let Some(body) = linker.pending.pop_front() else {
                // Every body is lowered. Each top-level function among them first runs its file's
                // initialization, which may queue the bodies of that initialization.
                linker
                    .initialize_files(&mut self.ir)
                    .map_err(|reason| KlibBodyDecline::new(identity.declaration.clone(), reason))?;
                if linker.pending.is_empty() {
                    break;
                }
                continue;
            };
            linker.active_module.clone_from(&body.identity.module);
            linker.active_identity.clone_from(&body.identity);
            linker.active_value_class.clone_from(&body.value_member);
            if let Some(member) = &body.value_member {
                linker.record_member_class(member);
            }
            let function = LoweredFunction {
                symbol: body.symbol,
                function: body.function,
            };
            let declined = |reason| match reason {
                KlibBodyDeclineReason::CalleeDeclined(decline) => *decline,
                reason => KlibBodyDecline::new(body.identity.declaration.clone(), reason),
            };
            if let Some((class, PendingInitialization::EnumEntries(arguments))) =
                &body.initialization
            {
                BodyLowering::new(
                    body.arena,
                    function,
                    &body.header,
                    &self.builtins,
                    &mut self.ir,
                    &mut linker,
                )
                .enum_entry_arguments(*class, arguments)
                .map_err(declined)?;
                continue;
            }
            // The secondary constructor a class initialization materialized, which carries its
            // own defaults.
            let initialized_secondary = linker
                .constructors
                .get(&body.identity)
                .and_then(|constructor| constructor.secondary);
            let lowering = BodyLowering::new(
                body.arena,
                function,
                &body.header,
                &self.builtins,
                &mut self.ir,
                &mut linker,
            );
            let lowered = if let Some((field, initializer)) = body.static_initialization {
                lowering.initialize_static(field, initializer)
            } else if let Some(local) = &body.local_body {
                match local {
                    super::same_library_class::LocalBody::Initialize(plan) => {
                        lowering.initialize_local(plan)
                    }
                    super::same_library_class::LocalBody::Member(member) => {
                        lowering.reading_local_fields(member).body(body.statements)
                    }
                }
                .and_then(|lowered| {
                    publish_parameter_defaults(
                        body.arena,
                        function,
                        &body.header,
                        &self.builtins,
                        &mut self.ir,
                        &mut linker,
                    )
                    .map(|()| lowered)
                })
            } else {
                match (&body.initialization, &body.value_member) {
                    (Some((class, PendingInitialization::Class(plan))), _) => {
                        lowering.initialize_class(*class, plan).and_then(|lowered| {
                            publish_constructor_defaults(
                                body.arena,
                                function,
                                &body.header,
                                &self.builtins,
                                &mut self.ir,
                                &mut linker,
                                (*class, initialized_secondary),
                            )
                            .map(|()| lowered)
                        })
                    }
                    (
                        Some((
                            class,
                            PendingInitialization::OwnDelegation {
                                secondary,
                                arguments,
                            },
                        )),
                        Some(member),
                    ) => lowering
                        .delegate_within_class(
                            *class,
                            *secondary,
                            member,
                            arguments,
                            body.statements,
                        )
                        .and_then(|lowered| {
                            publish_constructor_defaults(
                                body.arena,
                                function,
                                &body.header,
                                &self.builtins,
                                &mut self.ir,
                                &mut linker,
                                (*class, Some(*secondary)),
                            )
                            .map(|()| lowered)
                        }),
                    (Some((_, PendingInitialization::OwnDelegation { .. })), None) => {
                        Err(KlibBodyDeclineReason::SignatureMismatch(
                            "a delegation within the class has no materialized class".to_owned(),
                        ))
                    }
                    (Some((_, PendingInitialization::EnumEntries(_))), _) => {
                        unreachable!("an enum's entry frame is lowered before any function body")
                    }
                    (None, value_member) => match value_member {
                        Some(member) => lowering
                            .reading_through(
                                member.class,
                                member.declaring,
                                &member.fields,
                                member.properties.clone(),
                                member.contract.is_value,
                            )
                            .reading_outer_instance(member.class, member.outer)
                            .body(body.statements),
                        None => lowering.body(body.statements),
                    }
                    .and_then(|lowered| {
                        publish_parameter_defaults(
                            body.arena,
                            function,
                            &body.header,
                            &self.builtins,
                            &mut self.ir,
                            &mut linker,
                        )
                        .map(|()| lowered)
                    }),
                }
            }
            .map_err(declined)?;
            self.ir.functions[body.function as usize].body = Some(lowered);
        }
        let decline = |reason| KlibBodyDecline::new(identity.declaration.clone(), reason);
        super::class_hierarchies::publish(&mut self.ir, first_class, classes).map_err(decline)?;
        inline_functions::expand_calls(&mut self.ir).map_err(decline)?;
        super::atomic_references::resolve_all(&mut self.ir).map_err(decline)?;
        Ok(root)
    }
}

/// Publish a materialized constructor's parameter defaults, lowered in its initializer's frame,
/// where the constructor's default-argument entry reads them: the instance is value 0 and the
/// parameters follow it, as they do in the constructor's own frame. A primary constructor's
/// defaults are recorded beside its class, a secondary constructor's on its declaration.
fn publish_constructor_defaults(
    arena: &KlibIrArena,
    initializer: LoweredFunction<'_>,
    header: &FunctionHeader,
    builtins: &IrBuiltinOperators,
    ir: &mut IrFile,
    linker: &mut dyn CallLinker,
    (class, secondary): (crate::ir::ClassId, Option<usize>),
) -> Result<(), KlibBodyDeclineReason> {
    let Some(defaults) =
        lower_parameter_defaults(arena, initializer, header, builtins, ir, linker)?
    else {
        return Ok(());
    };
    match secondary {
        Some(ordinal) => ir.classes[class as usize].secondary_ctors[ordinal].defaults = defaults,
        None => {
            let classifier = ir.classes[class as usize].fq_name_id();
            ir.insert_class_ctor_defaults_name(classifier, defaults);
        }
    }
    Ok(())
}

/// Joins the calls of the bodies being lowered to the unit's functions, declaring each callee
/// reached for the first time and queueing its body.
struct Linker<'u, 's> {
    functions: &'u mut HashMap<KlibDependencyCallable, FunId>,
    classes: &'u mut HashMap<KlibDependencyCallable, crate::ir::ClassId>,
    constructors: &'u mut HashMap<KlibDependencyCallable, MaterializedConstructor>,
    realized_classes: &'u HashSet<TypeName>,
    adds_members_to_realized_classes: bool,
    authenticated_singleton_dispatches: &'u mut HashMap<KlibDependencyCallable, Ty>,
    authenticated_singleton_classifiers: &'u mut HashMap<KlibDependencyCallable, TypeName>,
    private_members: &'u mut HashMap<KlibDependencyCallable, OwnFormals>,
    member_receivers: &'u mut HashMap<KlibDependencyCallable, Ty>,
    local_classes: &'u mut HashMap<
        super::same_library_class::LocalIdentity,
        super::same_library_class::MaterializedLocalClass,
    >,
    local_functions: &'u mut HashMap<super::same_library_class::LocalIdentity, LocalFunction>,
    statics: &'u mut HashMap<static_properties::StaticIdentity, u32>,
    file_initializations: &'u mut HashMap<static_properties::FileIdentity, FunId>,
    initialized_functions: &'u mut HashSet<FunId>,
    presets_constant_statics: bool,
    pending: &'u mut VecDeque<PendingBody<'s>>,
    /// The declaration this lowering was asked for, which its own recursive calls select; `None`
    /// for an object materialized for its value.
    root: Option<KlibBodyCallable<'s>>,
    /// Declaring module of the body currently being lowered. Its producer dependency graph owns
    /// the origin of every public call symbol the body serialized.
    active_module: String,
    /// The body currently being lowered. A file-local class it materializes reports a failure as
    /// that body's decline, and later lowers its initializer in that body's library.
    active_identity: KlibDependencyCallable,
    /// The class and value field of the value class member being lowered, the class of every
    /// private member it calls.
    active_value_class: Option<MemberClass<'s>>,
    /// Each class whose member body this lowering has made active. A nested class's body calls a
    /// private member of its outer class through it.
    member_classes: Vec<MemberClass<'s>>,
    sources: Sources<'s>,
}

impl<'s> CallLinker for Linker<'_, 's> {
    fn is_suspend_callable(&self, target: ExternalCallableId) -> bool {
        self.external_is_suspend(target)
    }

    fn atomic_field(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<Option<crate::libraries::AtomicOperation>, KlibBodyDeclineReason> {
        Ok(self
            .linked_identity(callee)
            .and_then(|identity| self.sources.intrinsics.atomic_field(&identity)))
    }

    fn atomic_array_element(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<Option<crate::libraries::AtomicOperation>, KlibBodyDeclineReason> {
        Ok(self
            .linked_identity(callee)
            .and_then(|identity| self.sources.intrinsics.atomic_array_element(&identity)))
    }

    fn implicit_enum_member(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibDeclarationSignature,
    ) -> Result<Option<(TypeName, super::body_lowering::ImplicitEnumMember)>, KlibBodyDeclineReason>
    {
        self.implicit_enum_member_call(ir, callee)
    }

    fn common_primitive(
        &self,
        signature: &KlibDeclarationSignature,
    ) -> Result<Option<super::primitive_calls::CommonPrimitiveCall>, KlibBodyDeclineReason> {
        self.primitive_at(signature, true, |facts, identity, bodies| {
            facts.common_primitive(identity, bodies)
        })
    }

    fn intercepted_continuation(
        &self,
        signature: &KlibDeclarationSignature,
    ) -> Result<
        Option<super::intercepted_continuation::InterceptedContinuationExpansion>,
        KlibBodyDeclineReason,
    > {
        let bodies = self.sources.bodies;
        match bodies.linked_origin(&self.active_module, signature) {
            KlibCalleeOrigin::Exact(identity) => {
                super::intercepted_continuation::expansion(self.sources.callees, bodies, &identity)
            }
            KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => Ok(None),
        }
    }

    fn continuation_intrinsic(
        &self,
        signature: &KlibDeclarationSignature,
    ) -> Result<
        Option<super::continuation_intrinsics::ContinuationIntrinsicCall>,
        KlibBodyDeclineReason,
    > {
        let identity = match self
            .sources
            .bodies
            .linked_origin(&self.active_module, signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            // An intrinsic is a positive fact about one exact declaration; the call's own
            // linking reports the canonical producer-origin ambiguity.
            KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => return Ok(None),
        };
        self.sources.callees.continuation_intrinsic(&identity)
    }

    fn range_literal_bounds(
        &self,
        signature: &KlibDeclarationSignature,
    ) -> Result<Option<super::primitive_calls::CommonPrimitiveCall>, KlibBodyDeclineReason> {
        self.primitive_at(signature, false, |facts, identity, bodies| {
            facts.range_literal_bounds(identity, bodies)
        })
    }

    fn sam_declaration(
        &self,
        classifier: &KlibPublicIdSignature,
    ) -> Result<crate::ir::CheckedKlibSamDeclaration, KlibBodyDeclineReason> {
        self.sources
            .callees
            .sam_declaration(&self.active_module, classifier)
            .cloned()
            .ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a SAM conversion without a checked declaration",
            ))
    }

    fn declares_implementation(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<bool, KlibBodyDeclineReason> {
        let identity = self.callee_identity(callee)?;
        self.sources
            .bodies
            .function_has_body(&identity)
            .ok_or_else(|| KlibBodyDeclineReason::UnsupportedCallee(Box::new(callee.clone())))
    }

    fn range_membership_header(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<Option<super::body_lowering::RangeMembershipHeader>, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let identity = match self
            .sources
            .bodies
            .linked_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            // Dependency preparation omitted this declaration's edge, so a membership this unit
            // does not fold has no declaration to call: the body declines.
            KlibCalleeOrigin::Missing => {
                return Err(KlibBodyDeclineReason::UnsupportedCallee(Box::new(
                    callee.clone(),
                )))
            }
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(Box::new(
                    callee.clone(),
                )))
            }
        };
        let Some((arena, declaration)) = self.sources.bodies.function(&identity) else {
            return Err(KlibBodyDeclineReason::UnsupportedCallee(Box::new(
                callee.clone(),
            )));
        };
        let header = raw_function_header(None, HeaderPurpose::External, arena, declaration)?;
        let (params, ret, context_count, extension_receiver, receiver) = header.linked_signature();
        // The stdlib declares mixed-element `contains` extensions (`LongRange.contains(Int)`), a
        // valid declaration this fold does not model rather than inconsistent evidence.
        if context_count != 0 || extension_receiver {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a membership through a `contains` that is not a member of the range",
            ));
        }
        Ok(Some(super::body_lowering::RangeMembershipHeader {
            receiver,
            params: params.to_vec(),
            ret,
        }))
    }

    fn range_literal(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<Option<crate::fir::FirRangeOperation>, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let identity = match self
            .sources
            .bodies
            .callee_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            // Dependency preparation omitted this call's edge and the loop's protocol edges, so a
            // loop this unit does not count has nothing to call: the body declines.
            KlibCalleeOrigin::Missing => {
                return Err(KlibBodyDeclineReason::UnsupportedCallee(Box::new(
                    callee.clone(),
                )))
            }
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(Box::new(
                    callee.clone(),
                )))
            }
        };
        let operation = self
            .sources
            .declarations
            .external_callable_by_dependency(&identity)
            .and_then(|(_, realization)| {
                super::for_loops::range_literal_operation(realization.klib_body_callable()?)
            });
        match operation {
            Some(operation) => Ok(Some(operation)),
            None => Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a `for` loop over a progression that is not a range literal",
            )),
        }
    }

    fn indices_receiver(
        &self,
        getter: &KlibAccessorIdSignature,
    ) -> Result<Option<crate::libraries::IndicesReceiver>, KlibBodyDeclineReason> {
        let identity = self.indices_getter_identity(getter)?;
        Ok(self
            .sources
            .declarations
            .external_callable_by_dependency(&identity)
            .and_then(|(_, realization)| {
                super::for_loops::indices_receiver(realization.klib_body_callable()?)
            }))
    }

    fn indices_bound(
        &self,
        getter: &KlibAccessorIdSignature,
    ) -> Result<Option<super::body_lowering::IndicesBound>, KlibBodyDeclineReason> {
        let identity = self.indices_getter_identity(getter)?;
        let Some(bound) = self.sources.callees.indices_bound(&identity) else {
            return Ok(None);
        };
        let KlibDeclarationSignature::Accessor(accessor) = &bound.declaration else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "an `indices` bound is not a property accessor".to_owned(),
            ));
        };
        let selected = match self.sources.callees.get(bound) {
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for an `indices` bound".to_owned(),
                ))
            }
            None => return Err(KlibBodyDeclineReason::Unjoined),
        };
        Ok(Some(super::body_lowering::IndicesBound {
            accessor: accessor.clone(),
            ty: selected.ret(),
        }))
    }

    fn reverses_progression(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<bool, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let identity = match self
            .sources
            .bodies
            .callee_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing => {
                return Err(KlibBodyDeclineReason::UnsupportedOperation(
                    "a `for` loop over a `reversed` call without a provider declaration",
                ))
            }
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(Box::new(
                    callee.clone(),
                )))
            }
        };
        Ok(self
            .sources
            .declarations
            .external_callable_by_dependency(&identity)
            .is_some_and(|(_, realization)| {
                realization
                    .klib_body_callable()
                    .is_some_and(super::for_loops::reverses_progression)
            }))
    }

    fn local_class(
        &self,
        signature: &KlibIrSignature,
    ) -> Result<Option<TypeName>, KlibBodyDeclineReason> {
        super::same_library_class::classifier(self.sources.bodies, &self.active_module, signature)
    }

    fn site_container(&self) -> Option<crate::ir::IrSiteClassContainer> {
        super::site_containers::site_container(self.sources.bodies, &self.active_identity)
    }

    fn links_privately(&self, callee: &KlibIrSignature) -> bool {
        matches!(
            callee,
            KlibIrSignature::Public(_) | KlibIrSignature::Accessor(_)
        ) && (self
            .sources
            .bodies
            .module_local_function(&self.active_module, callee)
            .is_some()
            || self.unpublished_internal_function(callee).is_some())
    }

    fn link_local_object(
        &mut self,
        ir: &mut IrFile,
        signature: &KlibIrSignature,
    ) -> Result<TypeName, KlibBodyDeclineReason> {
        self.install_local_object(ir, signature)
    }

    fn is_local_interface(&self, signature: &KlibIrSignature) -> bool {
        self.names_local_interface(signature)
    }

    fn link_local_interface(
        &mut self,
        ir: &mut IrFile,
        signature: &KlibIrSignature,
    ) -> Result<(), KlibBodyDeclineReason> {
        self.install_local_interface(ir, signature)
    }

    fn implement_inherited_members(
        &mut self,
        ir: &mut IrFile,
        signature: &KlibIrSignature,
        owner: TypeName,
    ) -> Result<(), KlibBodyDeclineReason> {
        self.declare_inherited_members(ir, signature, owner)
    }

    fn link_local_constructor(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibIrSignature,
    ) -> Result<super::same_library_class::LocalConstructor, KlibBodyDeclineReason> {
        self.link_constructor_of_local_class(ir, callee)
    }

    fn inherited_getter(&self, callee: &KlibIrSignature) -> fake_overrides::Inherited {
        fake_overrides::inherited_getter(self, callee)
    }

    fn inherited_member(&self, callee: &KlibIrSignature) -> Option<KlibIrSignature> {
        fake_overrides::inherited_member(self, callee)
    }

    fn link_local_function(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibIrSignature,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        if let Some(identity) = self.unpublished_internal_function(callee) {
            return self.link_file_private(ir, identity.declaration.clone(), identity);
        }
        let &KlibIrSignature::FileLocal { file, slot } = callee else {
            // A member of a class private to its enclosing class keeps its public signature.
            if !self.links_privately(callee) {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a file-local call names a public signature".to_owned(),
                ));
            }
            return self.link_local_member(ir, callee);
        };
        let signature = KlibDeclarationSignature::FileLocal { file, slot };
        // A top-level function private to its file is the declaration of its own tree; every
        // other file-local function the library serializes is a member of a file-local class.
        match self
            .sources
            .bodies
            .linked_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(
                identity @ KlibDependencyCallable {
                    declaration: KlibDeclarationSignature::FileLocal { .. },
                    ..
                },
            ) => self.link_file_private(ir, signature, identity),
            // A fake override links to the member it inherits only as a call of that member
            // (`inherited_member`); any other use of it names no declaration of this module.
            KlibCalleeOrigin::Exact(_) | KlibCalleeOrigin::Missing => {
                self.link_local_member(ir, callee)
            }
            KlibCalleeOrigin::Ambiguous => Err(KlibBodyDeclineReason::SignatureMismatch(
                "a file-private call names two top-level declarations".to_owned(),
            )),
        }
    }

    fn is_subtype(
        &self,
        parameters: &super::klib_types::KlibTypeParameters,
        sub: Ty,
        sup: Ty,
    ) -> bool {
        let callees = self.sources.callees;
        let model = super::same_library_class::MaterializedClassModel {
            classes: self.local_classes,
            public: callees.class_type_oracle(),
        };
        model.is_subtype(parameters, sub, sup)
            || super::callable_references::function_supertype(sub, sup)
    }

    fn link_reference(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedCallableReference, KlibBodyDeclineReason> {
        let identity = self.callee_identity(callee)?;
        // A private member of a class this unit lowers has no provider declaration; the unit
        // emits it with the class, and the reference names that emitted member.
        if let Some(call) = self.private_class_member(ir, &identity)? {
            return private_members::private_member_reference(self.sources.bodies, &identity, call);
        }
        let (declaration, callable) = self.sources.callees.reference_declaration(&identity)?;
        let call = self.link(ir, callee)?;
        Ok(LinkedCallableReference::new(declaration, callable, call))
    }

    fn same_type(&self, left: Ty, right: Ty) -> bool {
        self.sources.callees.same_type(left, right)
    }

    fn classifier_parameter_bounds(&self, owner: TypeName, index: usize) -> Vec<Ty> {
        self.sources
            .callees
            .classifier_parameter_bounds(owner, index)
    }

    fn link_constructor(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
        role: ConstructorRole,
    ) -> Result<LinkedConstructor, KlibBodyDeclineReason> {
        self.link_selected_constructor(ir, callee, role)
    }

    fn constructor_bound(
        &self,
        parameters: &super::klib_types::KlibTypeParameters,
        argument: Ty,
        bound: Ty,
    ) -> bool {
        self.sources
            .callees
            .constructor_bound(parameters, argument, bound)
    }

    fn enum_entry(
        &mut self,
        ir: &mut IrFile,
        entry: &KlibPublicIdSignature,
    ) -> Result<(TypeName, String), KlibBodyDeclineReason> {
        let (classifier, name) = super::enum_classes::authenticate_entry(
            self.sources.bodies,
            &self.active_module,
            entry,
        )?;
        self.materialize_enum(ir, classifier)?;
        Ok((classifier, name))
    }

    fn static_field(
        &mut self,
        ir: &mut IrFile,
        field: &KlibIrSymbol,
    ) -> Result<Option<StaticField>, KlibBodyDeclineReason> {
        self.static_storage(ir, field)
    }

    fn singleton(
        &mut self,
        ir: &mut IrFile,
        classifier: TypeName,
    ) -> Result<(), KlibBodyDeclineReason> {
        // The target realizes the value of an object whose representation it owns, as it does for
        // a source reference of that object.
        // An object an earlier unit materialized is read through that unit's export, as its own
        // members are called.
        if self.sources.callees.target_represents(classifier)
            || self.realized_classes.contains(&classifier)
        {
            return Ok(());
        }
        // Any other library object is materialized with its instance, created by its own private
        // primary constructor, as a call of one of its members materializes it.
        let callees: &'s super::KlibCalleeFacts<'s> = self.sources.callees;
        let class =
            callees
                .read_class(classifier)
                .ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                    "an object value",
                ))?;
        let primary = class.primary_constructor.clone().ok_or(
            KlibBodyDeclineReason::UnsupportedOperation("an object value"),
        )?;
        let identity = KlibDependencyCallable {
            module: class.module.clone(),
            declaration: KlibDeclarationSignature::Public(primary),
        };
        self.materialize_declared_instances(ir, &identity, class)?;
        if !self.classes.contains_key(&identity) {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "an object value",
            ));
        }
        Ok(())
    }

    fn target_represents(&self, classifier: TypeName) -> bool {
        self.sources.callees.target_represents(classifier)
    }

    fn applied_supertype(&self, sub: Ty, owner: TypeName) -> Option<Ty> {
        let callees = self.sources.callees;
        super::same_library_class::applied_supertype(
            self.local_classes,
            sub,
            owner,
            &|sub, owner| callees.applied_supertype(sub, owner),
        )
    }

    fn sam_placement(&self) -> Result<crate::ir::IrSamPlacement, KlibBodyDeclineReason> {
        self.active_sam_placement()
    }

    fn dependency_override(
        &self,
        symbol: &KlibIrSymbol,
        supertypes: &[Ty],
        property: bool,
    ) -> Result<Option<super::same_library_class::MemberOverride>, KlibBodyDeclineReason> {
        let signature = match &symbol.signature {
            KlibIrSignature::Public(public) => KlibDeclarationSignature::Public(public.clone()),
            KlibIrSignature::Accessor(accessor) => {
                KlibDeclarationSignature::Accessor(accessor.clone())
            }
            KlibIrSignature::FileLocal { .. } | KlibIrSignature::TypeParameter { .. } => {
                return Ok(None)
            }
        };
        // The overridden symbol is the declaration itself, which this body's producer graph
        // resolves exactly as it resolves a call of it.
        let identity = match self
            .sources
            .bodies
            .linked_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing => return Ok(None),
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "two libraries serialize one overridden declaration".to_owned(),
                ))
            }
        };
        super::same_library_class::declared_override(
            identity,
            self.sources.declarations,
            supertypes,
            property,
            &|sub, owner| self.applied_supertype(sub, owner),
        )
    }

    fn external_override(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<ExternalCallableId, KlibBodyDeclineReason> {
        local_class::registered_override_handle(
            identity,
            self.sources.externals,
            self.sources.declarations,
        )
        .map(|handle| handle.target())
    }

    fn is_interface(&self, name: TypeName) -> Result<bool, KlibBodyDeclineReason> {
        inline_functions::is_interface(self, name)
    }

    fn link_accessor(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibAccessorIdSignature,
    ) -> Result<LinkedAccessorCall, KlibBodyDeclineReason> {
        let unsupported = || {
            KlibBodyDeclineReason::UnsupportedOperation(
                "a call through a property accessor signature",
            )
        };
        let signature = KlibDeclarationSignature::Accessor(callee.clone());
        let identity = match self
            .sources
            .bodies
            .linked_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing => return Err(unsupported()),
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(Box::new(
                    callee.property().clone(),
                )))
            }
        };
        // An accessor the unit emits is called as any function of the unit is: a member of a class
        // it materializes, as the class's other members are, or a top-level accessor.
        if let Some(linked) = self.installed_member(ir, &identity)? {
            return Ok(LinkedAccessorCall::Function(linked));
        }
        if let Some(linked) = self.materialized_member_accessor(ir, &identity)? {
            return Ok(LinkedAccessorCall::Function(linked));
        }
        if let Some(linked) = self.private_class_member(ir, &identity)? {
            return Ok(LinkedAccessorCall::Function(linked));
        }
        if let Some(linked) = self.top_level_accessor(ir, &identity)? {
            return Ok(LinkedAccessorCall::Function(linked));
        }
        // A complete retained export or positive target plan owns this edge. Other accessors
        // retain the explicit unsupported boundary instead of acquiring a header-only body path.
        if !self
            .sources
            .externals
            .get(&identity)
            .is_some_and(|external| external.realizes_accessor())
            && !self.is_earlier_abstract_accessor(&identity)
        {
            return Err(unsupported());
        }
        let selected = match self.sources.callees.get(&identity) {
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a getter declaration".to_owned(),
                ))
            }
            None => return Err(KlibBodyDeclineReason::Unjoined),
        };
        let mut linked = super::accessor_calls::authenticate_export(selected, self.sources.bodies)?
            .ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "a dependency property accessor without an exact accessor contract",
            ))?;
        // The exact external callable names an accessor's own formals in the substitutions its
        // read carries; an accessor without formals of its own needs no such name.
        if linked.type_parameters.is_empty() {
            return Ok(LinkedAccessorCall::Property(linked));
        }
        let (callable, realization) = self
            .sources
            .declarations
            .external_callable_by_dependency(&identity)
            .ok_or(KlibBodyDeclineReason::Unjoined)?;
        let selected_accessor = selected.accessor().ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "the selected accessor has no property identity".to_owned(),
            )
        })?;
        let realized_accessor = realization.klib_body_accessor.as_ref().ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "the selected external callable is not a property accessor".to_owned(),
            )
        })?;
        if realized_accessor.property != selected_accessor.property
            || realized_accessor.role != selected_accessor.role
        {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "the external callable resolves another accessor of the property".to_owned(),
            ));
        }
        linked.callable = Some(callable);
        Ok(LinkedAccessorCall::Property(linked))
    }

    fn requires_declaration(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<bool, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let identity = match self
            .sources
            .bodies
            .linked_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            // Compiler builtins such as `ieee754equals` deliberately have no KLIB declaration.
            // Absence therefore proves that no checked source edge selected this body callee.
            KlibCalleeOrigin::Missing => return Ok(false),
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(Box::new(
                    callee.clone(),
                )))
            }
        };
        if self.root.is_some_and(|root| &identity == root.identity()) {
            return Ok(true);
        }
        let selection = self.sources.callees.get(&identity);
        match selection {
            Some(KlibCalleeFact::Ambiguous) => Err(KlibBodyDeclineReason::AmbiguousCallee(
                Box::new(callee.clone()),
            )),
            _ => Ok(self.sources.callees.is_selected(&identity)
                || self.sources.externals.contains_key(&identity)),
        }
    }

    fn link_super(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        self.link_super_call(ir, callee)
    }

    fn link(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let identity = self.callee_identity(callee)?;
        // The active selection decides every call, including one of a function an earlier
        // lowering already put in the unit.
        let root = self.root.filter(|root| &identity == root.identity());
        let selected = if let Some(root) = &root {
            Some(SelectedKlibCallables::One(root))
        } else {
            match self.sources.callees.get(&identity) {
                None => None,
                Some(KlibCalleeFact::Ambiguous) => {
                    return Err(KlibBodyDeclineReason::AmbiguousCallee(Box::new(
                        callee.clone(),
                    )))
                }
                Some(KlibCalleeFact::Selected(callables)) => {
                    Some(SelectedKlibCallables::Variants(callables))
                }
            }
        };
        let callee_declined = |reason| {
            KlibBodyDeclineReason::CalleeDeclined(Box::new(KlibBodyDecline::new(
                signature.clone(),
                reason,
            )))
        };
        let (arena, declaration) = self
            .sources
            .bodies
            .function(&identity)
            .ok_or_else(|| callee_declined(KlibBodyDeclineReason::Unjoined))?;
        if let Some(intrinsic) = self.sources.intrinsics.get(&identity) {
            let selected = selected_callable_for_header_purpose(
                selected,
                HeaderPurpose::External,
                arena,
                declaration,
                Some(self.sources.bodies),
            )
            .map_err(callee_declined)?;
            self.cross_check_intrinsic(callee, selected, intrinsic)?;
            // A target intrinsic stays an ordinary call of its declaration; only the target's
            // emitter realizes it, through the provider fact frozen on the external handle. The
            // authenticated record, not the serialized header, fixes the call's shape.
            let Some(emitted) = self.sources.externals.get(&identity) else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an authenticated target intrinsic has no provider external handle".to_owned(),
                ));
            };
            return Ok(LinkedCallee {
                callee: Callee::External {
                    target: emitted.target(),
                    default_provider: None,
                    params: intrinsic.params().to_vec(),
                    ret: intrinsic.ret(),
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                    extension_receiver_parameter: None,
                },
                receiver: intrinsic.receiver(),
                params: intrinsic.params().to_vec(),
                ret: intrinsic.ret(),
                context_count: 0,
                singleton_dispatch: None,
                type_parameters: Vec::new(),
                type_parameter_bounds: Some(Vec::new()),
                class_type_parameters: self.class_type_parameters(selected)?,
                parameter_defaults: Vec::new(),
                edge: super::CallEdge::default(),
            });
        }
        if let Some(&emitted) = self.sources.externals.get(&identity) {
            if emitted.is_constructor() || emitted.constructor_initializer_target().is_some() {
                return Err(callee_declined(KlibBodyDeclineReason::SignatureMismatch(
                    "a constructor external is called as a function".to_owned(),
                )));
            }
            if let Some(operation) = emitted.target_intrinsic() {
                if self
                    .sources
                    .callees
                    .target_intrinsic(&identity, self.sources.bodies, &|found| found == operation)
                    .map_err(callee_declined)?
                    != Some(operation)
                {
                    return Err(callee_declined(KlibBodyDeclineReason::SignatureMismatch(
                        "a target leaf has no matching provider declaration".to_owned(),
                    )));
                }
            }
            let purpose = if emitted.target_intrinsic().is_some()
                || emitted.is_target_member()
                || declaration.body.is_none()
                || self.is_published_member(&identity, arena, declaration, selected.is_none())
            {
                HeaderPurpose::External
            } else {
                HeaderPurpose::Body(emitted.singleton_dispatch())
            };
            let selected = selected_callable_for_header_purpose(
                selected,
                purpose,
                arena,
                declaration,
                Some(self.sources.bodies),
            )
            .map_err(callee_declined)?;
            let (header, class_type_parameters) = self
                .outside_member_header(selected, purpose, arena, declaration)
                .map_err(callee_declined)?;
            let (params, ret, context_count, extension_receiver, receiver) =
                header.linked_signature();
            return Ok(LinkedCallee {
                callee: Callee::External {
                    target: emitted.target(),
                    default_provider: None,
                    params: params.to_vec(),
                    ret,
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                    extension_receiver_parameter: extension_receiver
                        .then_some(context_count as u32),
                },
                receiver,
                params: params.to_vec(),
                ret,
                context_count,
                singleton_dispatch: emitted.singleton_classifier(),
                type_parameters: selected
                    .and_then(|selected| selected.generic_signature())
                    .map_or_else(Vec::new, |signature| signature.formals.clone()),
                type_parameter_bounds: selected
                    .and_then(|selected| selected.generic_signature())
                    .map(|signature| signature.formal_bounds.clone()),
                class_type_parameters,
                parameter_defaults: Vec::new(),
                edge: if emitted.is_target_member() {
                    self.external_member_edge(&identity)
                        .map_err(callee_declined)?
                } else {
                    super::CallEdge::default()
                },
            });
        }
        if declaration.dispatch_receiver.is_some() && selected.is_none() {
            if let Some(linked) = self
                .private_class_member(ir, &identity)
                .map_err(callee_declined)?
            {
                return Ok(linked);
            }
        }
        if let Some(linked) = self
            .installed_member(ir, &identity)
            .map_err(callee_declined)?
        {
            return Ok(linked);
        }
        if declaration.dispatch_receiver.is_some() {
            // A member of a class this unit materializes dispatches statically and is emitted as a
            // member of that class. A member of a class realized outside the unit is called
            // through its frozen declaration, as a source call of it is.
            let candidates = match &selected {
                Some(SelectedKlibCallables::One(candidate)) => std::slice::from_ref(*candidate),
                Some(SelectedKlibCallables::Variants(candidates)) => *candidates,
                None => &[],
            };
            let mut materialized = !candidates.is_empty();
            for candidate in candidates {
                materialized &= self
                    .materializes_class_of(*candidate)
                    .map_err(callee_declined)?;
            }
            // An abstract member of a class an earlier unit materialized is a dispatch slot of
            // that unit's class, which no body here implements: the call dispatches through the
            // member's frozen declaration, as a source call of it does.
            if materialized && declaration.body.is_none() && !self.functions.contains_key(&identity)
            {
                let mut realized_earlier = true;
                for candidate in candidates {
                    realized_earlier &= self.class_realized_earlier(*candidate);
                }
                materialized = !realized_earlier;
            }
            if !materialized {
                // Without a frozen declaration and call edge, which a target realizing the member
                // outside the unit has, the call itself is the unsupported form.
                if self.sources.callees.reference_target(&identity).is_none()
                    || self.sources.callees.member_edge(&identity).is_none()
                {
                    return Err(KlibBodyDeclineReason::UnsupportedOperation(
                        "a call of a member function",
                    ));
                }
                return self
                    .external_member(&identity, selected, arena, declaration)
                    .map_err(callee_declined);
            }
            let selected = selected_callable_for_header_purpose(
                selected,
                HeaderPurpose::Body(None),
                arena,
                declaration,
                Some(self.sources.bodies),
            )
            .map_err(callee_declined)?
            .expect("a value class member call has a selected declaration");
            return self
                .value_member(ir, &identity, selected, arena, declaration)
                .map_err(callee_declined);
        }
        self.top_level_function(ir, &identity, selected, arena, declaration)
            .map_err(callee_declined)
    }
}

impl<'s> Linker<'_, 's> {
    /// A top-level function the unit emits, linked under the active selection: an ordinary
    /// function or a top-level property's accessor, generic or not. A call of it binds the
    /// selected declaration's own type parameters, as a call of any function the unit lowers does.
    fn top_level_function(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: Option<SelectedKlibCallables<'s, '_>>,
        arena: &'s KlibIrArena,
        declaration: &'s KlibIrFunction,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let selected = selected_callable_for_header(selected, None, arena, declaration)?;
        let function = self.top_level_callee(ir, identity, selected, arena, declaration)?;
        let parameter_defaults = parameter_defaults(declaration);
        let declaration = &ir.functions[function as usize];
        Ok(LinkedCallee {
            callee: Callee::Local(function),
            receiver: None,
            params: declaration.params.clone(),
            ret: declaration.ret,
            context_count: ir.declared_parameters(function).context_count,
            singleton_dispatch: None,
            type_parameters: selected
                .and_then(|selected| selected.generic_signature())
                .map_or_else(Vec::new, |signature| signature.formals.clone()),
            type_parameter_bounds: selected
                .and_then(|selected| selected.generic_signature())
                .map(|signature| signature.formal_bounds.clone()),
            class_type_parameters: Vec::new(),
            parameter_defaults,
            edge: super::CallEdge::default(),
        })
    }

    /// The accessor of a property of a class whose members the unit emits, when it has a body or
    /// is abstract.
    fn materialized_member_accessor(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<LinkedCallee>, KlibBodyDeclineReason> {
        if self.sources.externals.contains_key(identity) {
            return Ok(None);
        }
        let Some(KlibCalleeFact::Selected(callables)) = self.sources.callees.get(identity) else {
            return Ok(None);
        };
        let selected = callables[0];
        if selected.accessor().is_none()
            || !selected
                .class_contract()
                .is_some_and(crate::libraries::KlibBodyClassContract::emits_members)
        {
            return Ok(None);
        }
        let Some((arena, declaration)) = self.sources.bodies.function(identity) else {
            return Ok(None);
        };
        // An abstract accessor is declared with its class, as an abstract member function is,
        // and a call of it dispatches to the instance's override.
        let abstract_ = is_abstract_member(declaration);
        if !abstract_ && !matches!(declaration.body, Some(KlibIrBody::Block(_))) {
            return Ok(None);
        }
        // An abstract accessor of a class an earlier unit realized is that unit's dispatch slot,
        // which no body here implements.
        if abstract_
            && !self.functions.contains_key(identity)
            && self.class_realized_earlier(selected)
        {
            return Ok(None);
        }
        let selected = selected_callable_for_header_purpose(
            Some(SelectedKlibCallables::Variants(callables)),
            HeaderPurpose::Body(None),
            arena,
            declaration,
            Some(self.sources.bodies),
        )?
        .expect("a selected bucket holds a callable");
        self.value_member(ir, identity, selected, arena, declaration)
            .map(Some)
    }

    /// Whether this unit materializes the class declaring the member `candidate`: a value class,
    /// or an ordinary class whose primary constructor's initialization the class plan admits.
    fn materializes_class_of(
        &self,
        candidate: KlibBodyCallable<'s>,
    ) -> Result<bool, KlibBodyDeclineReason> {
        if is_receiver_member(candidate, self.sources.bodies, self.sources.callees) {
            // A member realizing one of `kotlin.Any`'s stays the target's, as the closure froze it.
            return Ok(!self
                .sources
                .callees
                .member_edge(candidate.identity())
                .is_some_and(super::CallEdge::realizes_any_member));
        }
        let Some(class) = candidate
            .class_contract()
            .filter(|class| class.emits_members())
        else {
            return Ok(false);
        };
        if class.is_value {
            return Ok(true);
        }
        if class.declares_instances() {
            return Ok(super::class_initialization::declared_instances_plan(
                class,
                self.sources.bodies,
                self.sources.callees,
            )?
            .is_some());
        }
        // Every instance a member is called on was created by one of the constructors that
        // initialize the class, so the unit materializes the class only when each of them does.
        let Some(constructors) =
            super::class_initialization::initializing_constructors(class, self.sources.bodies)
        else {
            return Ok(false);
        };
        for identity in &constructors {
            let Some(KlibCalleeFact::Selected(selected)) = self.sources.callees.get(identity)
            else {
                return Ok(false);
            };
            if super::class_initialization::plan(
                selected[0],
                self.sources.bodies,
                self.sources.callees,
            )?
            .is_none()
            {
                return Ok(false);
            }
        }
        Ok(!constructors.is_empty())
    }

    /// A call of a member of a class this unit does not materialize, through the member's
    /// declaration frozen with the unit's closure. The target realizes and dispatches it as it does
    /// a source call of the same declaration.
    fn external_member(
        &self,
        identity: &KlibDependencyCallable,
        selected: Option<SelectedKlibCallables<'s, '_>>,
        arena: &'s KlibIrArena,
        declaration: &'s KlibIrFunction,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let target = self
            .sources
            .callees
            .reference_target(identity)
            .expect("the caller proved a frozen declaration");
        let selected = selected_callable_for_header_purpose(
            selected,
            HeaderPurpose::External,
            arena,
            declaration,
            Some(self.sources.bodies),
        )?
        .ok_or(KlibBodyDeclineReason::UnsupportedOperation(
            "a call of a member function without a provider declaration",
        ))?;
        let (header, class_type_parameters) = self.outside_member_header(
            Some(selected),
            HeaderPurpose::External,
            arena,
            declaration,
        )?;
        let (params, ret, context_count, extension_receiver, receiver) = header.linked_signature();
        Ok(LinkedCallee {
            callee: Callee::External {
                target,
                default_provider: None,
                params: params.to_vec(),
                ret,
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: extension_receiver.then_some(context_count as u32),
            },
            receiver,
            params: params.to_vec(),
            ret,
            context_count,
            singleton_dispatch: None,
            type_parameters: selected
                .generic_signature()
                .map_or_else(Vec::new, |signature| signature.formals.clone()),
            type_parameter_bounds: selected
                .generic_signature()
                .map(|signature| signature.formal_bounds.clone()),
            class_type_parameters,
            parameter_defaults: Vec::new(),
            edge: self.external_member_edge(identity)?,
        })
    }

    /// The header of `declaration`, a callable realized outside this unit (by the target, by its
    /// provider, or by an earlier unit), with the formals of its class. A member of a generic
    /// class is read at its class's type arguments, which the call binds from its dispatch
    /// receiver.
    fn outside_member_header(
        &self,
        selected: Option<KlibBodyCallable<'s>>,
        purpose: HeaderPurpose,
        arena: &'s KlibIrArena,
        declaration: &'s KlibIrFunction,
    ) -> Result<(FunctionHeader, Vec<String>), KlibBodyDeclineReason> {
        let class = match selected {
            Some(selected)
                if selected
                    .class_contract()
                    .is_some_and(|class| !class.parameters.formals.is_empty()) =>
            {
                Some(super::class_headers::authenticate(
                    selected,
                    self.sources.bodies,
                )?)
            }
            _ => None,
        };
        match class {
            Some(class) => {
                super::class_headers::authenticate_receiver(
                    class.contract,
                    class.arena,
                    class.function,
                )?;
                let header = super::function_lowering::raw_function_header_with_class(
                    selected,
                    purpose,
                    class.arena,
                    class.function,
                    class.types,
                )?;
                Ok((header, class.formals))
            }
            None => Ok((
                raw_function_header(selected, purpose, arena, declaration)?,
                Vec::new(),
            )),
        }
    }

    /// The facts a call of the member `identity`, which a target realizes, carries, as the
    /// unit's closure froze them from the member hierarchy.
    fn external_member_edge(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<super::CallEdge, KlibBodyDeclineReason> {
        self.sources
            .callees
            .member_edge(identity)
            .cloned()
            .ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "a member realized outside the unit has no frozen call edge".to_owned(),
                )
            })
    }

    /// The exact declaration of the `indices` getter a loop calls, which dependency preparation
    /// reached as the loop's edge.
    fn indices_getter_identity(
        &self,
        getter: &KlibAccessorIdSignature,
    ) -> Result<KlibDependencyCallable, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Accessor(getter.clone());
        match self
            .sources
            .bodies
            .callee_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => Ok(identity),
            // Dependency preparation omitted this getter's edge: the loop is not one this unit counts.
            KlibCalleeOrigin::Missing => Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a `for` loop over an `indices` getter without a provider declaration",
            )),
            KlibCalleeOrigin::Ambiguous => Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(
                Box::new(getter.property().clone()),
            )),
        }
    }

    /// Whether `identity` is an abstract accessor of a class an earlier unit realized and this
    /// unit does not declare: a call of it dispatches through the member's frozen declaration.
    fn is_earlier_abstract_accessor(&self, identity: &KlibDependencyCallable) -> bool {
        let Some(KlibCalleeFact::Selected(callables)) = self.sources.callees.get(identity) else {
            return false;
        };
        let Some((_, declaration)) = self.sources.bodies.function(identity) else {
            return false;
        };
        is_abstract_member(declaration)
            && !self.functions.contains_key(identity)
            && self.class_realized_earlier(callables[0])
            && self.sources.callees.reference_target(identity).is_some()
    }

    /// A call of the value class member `selected`, which this unit emits as a member of its
    /// materialized class.
    fn value_member(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: KlibBodyCallable<'s>,
        arena: &'s KlibIrArena,
        declaration: &'s KlibIrFunction,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let function = match self.functions.get(identity) {
            Some(&function) => {
                body_header(Some(selected), arena, declaration, self.sources.bodies)?;
                function
            }
            // An abstract member is a dispatch slot of its class, which declares it when the
            // unit materializes the class; a call can reach it before any construction does.
            None if is_abstract_member(declaration) => {
                self.materialize_member_class(ir, selected)?;
                *self
                    .functions
                    .get(identity)
                    .ok_or(KlibBodyDeclineReason::NoBody)?
            }
            None => self.declare_callee(ir, identity, Some(selected), arena, declaration)?,
        };
        let declared = &ir.functions[function as usize];
        let (Some(receiver), params) = (
            self.member_receivers.get(identity).copied(),
            &declared.params,
        ) else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a value class member is emitted without its receiver".to_owned(),
            ));
        };
        Ok(LinkedCallee {
            callee: Callee::Local(function),
            receiver: Some(receiver),
            params: params.to_vec(),
            ret: declared.ret,
            context_count: 0,
            singleton_dispatch: None,
            type_parameters: selected
                .generic_signature()
                .map_or_else(Vec::new, |signature| signature.formals.clone()),
            type_parameter_bounds: selected
                .generic_signature()
                .map(|signature| signature.formal_bounds.clone()),
            class_type_parameters: self.class_type_parameters(Some(selected))?,
            parameter_defaults: parameter_defaults(declaration),
            edge: super::CallEdge::default(),
        })
    }

    fn class_type_parameters(
        &self,
        selected: Option<KlibBodyCallable<'s>>,
    ) -> Result<Vec<String>, KlibBodyDeclineReason> {
        match selected {
            Some(selected)
                if selected
                    .class_contract()
                    .is_some_and(|class| !class.parameters.formals.is_empty()) =>
            {
                Ok(super::class_headers::authenticate(selected, self.sources.bodies)?.formals)
            }
            _ => Ok(Vec::new()),
        }
    }

    fn callee_identity(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<KlibDependencyCallable, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        match self
            .sources
            .bodies
            .linked_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => Ok(identity),
            KlibCalleeOrigin::Ambiguous => Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(
                Box::new(callee.clone()),
            )),
            KlibCalleeOrigin::Missing if callee.declaration().segments().len() > 1 => {
                // A member no library serializes is, in a complete library set, a fake override
                // whose declared hierarchy did not bind it to one real declaration.
                Err(KlibBodyDeclineReason::UnsupportedOperation(
                    "a call of a member no library serializes, such as a fake override",
                ))
            }
            KlibCalleeOrigin::Missing => Err(KlibBodyDeclineReason::UndeclaredCallee(Box::new(
                callee.clone(),
            ))),
        }
    }

    fn cross_check_intrinsic(
        &self,
        callee: &KlibPublicIdSignature,
        selected: Option<KlibBodyCallable<'s>>,
        intrinsic: &KlibIntrinsicCallable,
    ) -> Result<(), KlibBodyDeclineReason> {
        let Some(selected) = selected else {
            return Ok(());
        };
        if selected.compiler_intrinsic() != Some(intrinsic.operation())
            || selected.kind() != intrinsic.kind()
            || selected.params() != intrinsic.params()
            || selected.ret() != intrinsic.ret()
        {
            return Err(KlibBodyDeclineReason::CalleeDeclined(Box::new(
                KlibBodyDecline::new(
                    KlibDeclarationSignature::Public(callee.clone()),
                    KlibBodyDeclineReason::SignatureMismatch(
                        "an authenticated intrinsic disagrees with its selected provider record"
                            .to_owned(),
                    ),
                ),
            )));
        }
        Ok(())
    }

    /// Declare serialized `function`, carrying `identity` and described also by the
    /// provider record `selected` when there is one, and queue its body.
    fn declare(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: Option<KlibBodyCallable<'s>>,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        host_imports::require_body(arena, function)?;
        let header = body_header(selected, arena, function, self.sources.bodies)?;
        self.declare_with_header(ir, identity, selected, arena, function, header)
    }

    /// Declare a declaration no provider records, whose formals `types` already binds.
    fn declare_with_types(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: Option<KlibBodyCallable<'s>>,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
        types: super::klib_types::KlibTypeParameters,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        host_imports::require_body(arena, function)?;
        let header = super::function_lowering::raw_function_header_with_class(
            selected,
            HeaderPurpose::Body(None),
            arena,
            function,
            types,
        )?;
        self.declare_with_header(ir, identity, selected, arena, function, header)
    }

    fn declare_with_header(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: Option<KlibBodyCallable<'s>>,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
        header: super::function_lowering::FunctionHeader,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        let value_member = match (header.value_receiver(), selected) {
            // A builtin type's member reads its receiver as the value the target carries.
            (Some(_), Some(selected))
                if is_receiver_member(selected, self.sources.bodies, self.sources.callees)
                    || selected.class_contract().is_some_and(
                        crate::libraries::KlibBodyClassContract::emits_default_members,
                    ) =>
            {
                None
            }
            // A singleton edge reads the object it dispatches to, which its receiver proof
            // selects; no class is materialized for it.
            (Some(_), Some(selected)) if selected.singleton_dispatch().is_some() => None,
            (Some(_), Some(selected)) => Some(self.materialize_member_class(ir, selected)?),
            (Some(_), None) => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a class member body has no selected declaration".to_owned(),
                ))
            }
            (None, _) => None,
        };
        // Materializing a value class declares its overrides, this member among them.
        if let Some(&declared) = self.functions.get(identity) {
            return Ok(declared);
        }
        let class = value_member.as_ref().map(|member| member.class);
        let declared = self.queue(
            ir,
            identity,
            selected,
            arena,
            function,
            BodyHeader {
                header,
                value_member,
            },
        )?;
        if let Some(class) = class {
            register_class_method(ir, class, declared);
        }
        Ok(declared)
    }

    /// Declare `function` with `header` and queue its body.
    fn queue(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
        selected: Option<KlibBodyCallable<'s>>,
        arena: &'s KlibIrArena,
        function: &'s KlibIrFunction,
        BodyHeader {
            header,
            value_member,
        }: BodyHeader<'s>,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        // A host function has no body to queue; the target imports it.
        if let Some(declared) = host_imports::declare(ir, arena, function, &header) {
            self.functions.insert(identity.clone(), declared);
            return Ok(declared);
        }
        let statements = block_statements(function)?;
        let declared = declare_function(ir, &header);
        let callee = inline_functions::Candidate::new(declared, function, &header, selected);
        callee.declare(ir, self.root, identity, &mut |name| self.is_interface(name))?;
        callee.register_suspension(ir);
        self.functions.insert(identity.clone(), declared);
        if let Some(formals) = self.private_members.get_mut(identity) {
            *formals = header.own_formals(function);
        }
        if let Some(receiver) = header.value_receiver() {
            self.member_receivers.insert(identity.clone(), receiver);
        }
        if let Some(singleton) = selected.and_then(KlibBodyCallable::singleton_dispatch) {
            self.authenticated_singleton_dispatches
                .insert(identity.clone(), singleton);
        }
        if let Some(classifier) = selected.and_then(KlibBodyCallable::singleton_classifier) {
            self.authenticated_singleton_classifiers
                .insert(identity.clone(), classifier);
        }
        self.pending.push_back(PendingBody {
            identity: identity.clone(),
            arena,
            symbol: &function.base.symbol,
            statements,
            header,
            function: declared,
            initialization: None,
            value_member,
            local_body: None,
            static_initialization: None,
        });
        Ok(declared)
    }
}

/// Whether `candidate` is a member with a body of a builtin type whose members a support unit emits
/// as functions of their receiver value
/// ([`emits_receiver_members`](crate::libraries::KlibBodyClassContract::emits_receiver_members))
/// and whose layout the target does not own. A bodyless one is the target's to realize.
pub(crate) fn is_receiver_member(
    candidate: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &KlibCalleeFacts<'_>,
) -> bool {
    candidate.is_member()
        && candidate
            .class_contract()
            .is_some_and(crate::libraries::KlibBodyClassContract::emits_receiver_members)
        && bodies.function_has_body(candidate.identity()) == Some(true)
        // The body of a member of a type the target lays out itself reads a layout the target's
        // value does not have.
        && !matches!(
            candidate.declaration_owner(),
            Some(crate::types::SemanticCallableOwner::Classifier(owner))
                if callees.target_represents(owner)
        )
}

/// Record `function` as a method of the materialized `class`, once. A member no class table
/// numbers (a final or private one) still belongs to its class, so a call of it runs it directly.
fn register_class_method(ir: &mut IrFile, class: crate::ir::ClassId, function: FunId) {
    if !ir.classes[class as usize].methods.contains(&function) {
        ir.classes[class as usize].methods.push(function);
        ir.note_class_method(class, function);
    }
}

/// The statements of the body `function` serializes.
fn block_statements(
    function: &KlibIrFunction,
) -> Result<&[KlibIrStatement], KlibBodyDeclineReason> {
    match &function.body {
        None => Err(KlibBodyDeclineReason::NoBody),
        Some(KlibIrBody::Synthetic(_)) => Err(KlibBodyDeclineReason::SyntheticBody),
        Some(KlibIrBody::Block(statements)) => Ok(statements),
    }
}

/// The header of a body this unit emits. A member of a generic class names its class's type
/// parameters, bound from the class's own header.
fn body_header(
    selected: Option<KlibBodyCallable<'_>>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    bodies: &KlibDeclarationBodies,
) -> Result<super::function_lowering::FunctionHeader, KlibBodyDeclineReason> {
    match selected.filter(|selected| {
        selected
            .class_contract()
            .is_some_and(|class| !class.parameters.formals.is_empty())
    }) {
        Some(member) => {
            let class = super::class_headers::authenticate(member, bodies)?;
            super::function_lowering::raw_function_header_with_class(
                Some(member),
                HeaderPurpose::Body(None),
                arena,
                function,
                class.types,
            )
        }
        None => function_header(selected, arena, function),
    }
}

/// Select the exact checked edge variant that agrees with one serialized declaration. Several
/// singleton objects may inherit the same declaration identity, but only the variant whose
/// semantic receiver matches this body (and an already-emitted contract, when present) can
/// authenticate the call. Declaration-shape conflicts remain ambiguous in `KlibCalleeFacts`.
#[derive(Clone, Copy)]
enum SelectedKlibCallables<'callable, 'facts> {
    One(&'facts KlibBodyCallable<'callable>),
    Variants(&'facts [KlibBodyCallable<'callable>]),
}

fn selected_callable_for_header<'callable>(
    candidates: Option<SelectedKlibCallables<'callable, '_>>,
    emitted_singleton_dispatch: Option<Ty>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<Option<KlibBodyCallable<'callable>>, KlibBodyDeclineReason> {
    selected_callable_for_header_purpose(
        candidates,
        HeaderPurpose::Body(emitted_singleton_dispatch),
        arena,
        function,
        None,
    )
}

fn selected_callable_for_header_purpose<'callable>(
    candidates: Option<SelectedKlibCallables<'callable, '_>>,
    purpose: HeaderPurpose,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    class_bodies: Option<&KlibDeclarationBodies>,
) -> Result<Option<KlibBodyCallable<'callable>>, KlibBodyDeclineReason> {
    let Some(candidates) = candidates else {
        return Ok(None);
    };
    let mut selected = None;
    let mut first_mismatch = None;
    let header = |candidate: KlibBodyCallable<'callable>| {
        super::function_lowering::selected_function_header(
            candidate,
            purpose,
            arena,
            function,
            class_bodies,
        )
    };
    let mut consider = |candidate| {
        match header(candidate) {
            Ok(_) if selected.is_none() => selected = Some(candidate),
            Ok(_) => return Some(KlibBodyDeclineReason::SignatureMismatch(
                "more than one checked call-edge variant agrees with the serialized declaration"
                    .to_owned(),
            )),
            Err(reason) if first_mismatch.is_none() => first_mismatch = Some(reason),
            Err(_) => {}
        }
        None
    };
    match candidates {
        SelectedKlibCallables::One(&candidate) => {
            if let Some(reason) = consider(candidate) {
                return Err(reason);
            }
        }
        SelectedKlibCallables::Variants(candidates) => {
            for &candidate in candidates {
                if let Some(reason) = consider(candidate) {
                    return Err(reason);
                }
            }
        }
    }
    selected.map(Some).ok_or_else(|| {
        first_mismatch.unwrap_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "a selected KLIB declaration has no checked call-edge variant".to_owned(),
            )
        })
    })
}

/// What a unit held before a lowering began: its IR ([`LoweringCheckpoint`]) and the identities
/// joined to the functions that IR declares.
struct UnitCheckpoint(LoweringCheckpoint);

impl UnitCheckpoint {
    fn of(unit: &DependencyBodyUnit) -> Self {
        Self(LoweringCheckpoint::of(&unit.ir))
    }

    fn restore(self, unit: &mut DependencyBodyUnit) {
        let first = self.0.restore(&mut unit.ir);
        unit.functions.retain(|_, function| *function < first);
        // A constructor this lowering declared published its identity, even when it joined a
        // class materialized before.
        let constructors = &unit.constructors;
        unit.classes.retain(|identity, class| {
            (*class as usize) < unit.ir.classes.len()
                && constructors
                    .get(identity)
                    .is_none_or(|constructor| constructor.initializer < first)
        });
        let classes = &unit.classes;
        unit.constructors.retain(|identity, constructor| {
            constructor.initializer < first && classes.contains_key(identity)
        });
        let functions = &unit.functions;
        unit.authenticated_singleton_dispatches
            .retain(|identity, _| functions.contains_key(identity));
        unit.authenticated_singleton_classifiers
            .retain(|identity, _| functions.contains_key(identity));
        unit.private_members
            .retain(|identity, _| functions.contains_key(identity));
        unit.member_receivers
            .retain(|identity, _| functions.contains_key(identity));
        unit.local_classes
            .retain(|_, class| (class.class as usize) < unit.ir.classes.len());
        unit.local_functions
            .retain(|_, function| (function.function as usize) < unit.ir.functions.len());
        unit.statics
            .retain(|_, index| (*index as usize) < unit.ir.statics.len());
        unit.file_initializations
            .retain(|_, function| *function < first);
        unit.initialized_functions
            .retain(|function| *function < first);
        unit.ir.function_overrides.retain(|owner, _| {
            unit.ir
                .classes
                .iter()
                .any(|class| class.fq_name_id() == *owner)
        });
    }
}

/// Whether each value parameter of `declaration`, in the order its lowered function declares
/// them, has a serialized default a call may leave it to.
fn parameter_defaults(declaration: &KlibIrFunction) -> Vec<bool> {
    declaration
        .context_parameters
        .iter()
        .chain(&declaration.extension_receiver)
        .chain(&declaration.regular_parameters)
        .map(|parameter| parameter.default_value.is_some())
        .collect()
}

impl Linker<'_, '_> {
    /// The exact declaration a call of `callee` from the active module links to, if one does.
    fn linked_identity(&self, callee: &KlibPublicIdSignature) -> Option<KlibDependencyCallable> {
        match self.sources.bodies.linked_origin(
            &self.active_module,
            &KlibDeclarationSignature::Public(callee.clone()),
        ) {
            KlibCalleeOrigin::Exact(identity) => Some(identity),
            KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
        }
    }

    /// The primitive `select` reads for the declaration a call of `signature` names, unless the
    /// call is an exact declaration edge.
    fn primitive_at(
        &self,
        signature: &KlibDeclarationSignature,
        allow_target_property_read: bool,
        select: PrimitiveSelection,
    ) -> Result<Option<super::primitive_calls::CommonPrimitiveCall>, KlibBodyDeclineReason> {
        // Keep the existing public-call obligation and canonical ambiguity diagnostics. A
        // positive primitive capability does not own selection or producer-origin failures.
        if let KlibDeclarationSignature::Public(callee) = signature {
            if self.requires_declaration(callee)? {
                return Ok(None);
            }
        }
        let identity = match self
            .sources
            .bodies
            .linked_origin(&self.active_module, signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing => return Ok(None),
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "ambiguous producer origin for a primitive declaration".to_owned(),
                ))
            }
        };
        let selected = select(self.sources.callees, &identity, self.sources.bodies)?;
        let primitive = if let Some(external) = self.sources.externals.get(&identity) {
            let property_read = external.target_intrinsic().filter(|operation| {
                allow_target_property_read
                    && matches!(
                        operation,
                        crate::libraries::CompilerIntrinsic::ArraySize
                            | crate::libraries::CompilerIntrinsic::StringLength
                    )
            });
            property_read
                .map(|operation| {
                    self.sources.callees.target_property_read(
                        &identity,
                        self.sources.bodies,
                        operation,
                    )
                })
                .transpose()?
                .flatten()
        } else {
            selected
        };
        Ok(
            if self.root.is_some_and(|root| &identity == root.identity()) {
                None
            } else {
                primitive
            },
        )
    }
}

/// A declaration's own type parameters, each with its declared upper bounds (empty for the
/// implicit `Any?`), in declaration order.
type OwnFormals = Vec<(crate::types::TyParamName, Vec<Ty>)>;

/// A primitive reading of a selected declaration's callee facts.
type PrimitiveSelection = for<'f> fn(
    &KlibCalleeFacts<'f>,
    &KlibDependencyCallable,
    &KlibDeclarationBodies,
) -> Result<
    Option<super::primitive_calls::CommonPrimitiveCall>,
    KlibBodyDeclineReason,
>;

/// Whether `declaration` is an abstract member: a dispatch slot its class declares, with no body.
fn is_abstract_member(declaration: &KlibIrFunction) -> bool {
    declaration.body.is_none()
        && crate::metadata::semantic::declaration_modality(declaration.base.flags)
            == crate::metadata::semantic::KotlinModality::Abstract
}
