//! The common-IR unit that holds the lowered bodies of selected dependency declarations.
//!
//! Lowering one selected declaration also lowers every dependency declaration its body calls,
//! transitively, so the unit is closed over calls: each call names a function of the same unit.
//! A callee is joined to its serialized declaration by exact module-qualified identity, as a body
//! is, and declared from that declaration's header; a provider record of the same identity, when a
//! checked call selected one, is cross-checked against the header. A declaration is lowered once
//! per module-qualified signature, and a call cycle links back to the function it already declared.
//! If any reached body declines, the unit is left exactly as it was.

use crate::fx_hash::HashMap;
use std::collections::VecDeque;

use super::body_lowering::{
    BodyLowering, CallLinker, LinkedAccessor, LinkedCallee, LinkedConstructor, LoweredFunction,
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
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrBody, KlibIrFunction, KlibIrStatement};
use crate::metadata::klib_ir::KlibIrSymbol;
use crate::types::{Ty, TypeName};

mod class_construction;

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

    fn realizes_getter(self) -> bool {
        matches!(
            self.realization,
            DependencyExternalRealization::EmittedBody
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
        matches!(self.realization, DependencyExternalRealization::Constructor)
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
    authenticated_singleton_dispatches: HashMap<KlibDependencyCallable, Ty>,
    authenticated_singleton_classifiers: HashMap<KlibDependencyCallable, TypeName>,
    builtins: IrBuiltinOperators,
}

impl Default for DependencyBodyUnit {
    fn default() -> Self {
        Self {
            ir: IrFile::default(),
            functions: HashMap::default(),
            classes: HashMap::default(),
            authenticated_singleton_dispatches: HashMap::default(),
            authenticated_singleton_classifiers: HashMap::default(),
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
}

/// A declared function whose body is still to be lowered.
struct PendingBody<'a> {
    identity: KlibDependencyCallable,
    arena: &'a KlibIrArena,
    symbol: &'a KlibIrSymbol,
    statements: &'a [KlibIrStatement],
    header: FunctionHeader,
    function: FunId,
    initialization: Option<(
        crate::ir::ClassId,
        super::class_initialization::ClassInitialization<'a, 'a>,
    )>,
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
        if let Some(function) = self.functions.get(callable.identity()).copied() {
            // Declared before, perhaps from its header alone as another body's callee: the
            // selection it is now asked for must describe it too.
            let (arena, declaration) = bodies.function(callable.identity()).ok_or_else(|| {
                KlibBodyDecline::new(
                    callable.signature().clone(),
                    KlibBodyDeclineReason::Unjoined,
                )
            })?;
            function_header(Some(callable), arena, declaration)
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
    pub(crate) fn lower_root_with_intrinsics_and_externals<'a>(
        &mut self,
        root: KlibBodyCallable<'a>,
        bodies: &'a KlibDeclarationBodies,
        callees: &'a KlibCalleeFacts<'a>,
        intrinsics: &'a KlibIntrinsicFacts,
        externals: &'a HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
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
        let (arena, declaration) = bodies.function(root.identity()).ok_or_else(|| {
            KlibBodyDecline::new(root.signature().clone(), KlibBodyDeclineReason::Unjoined)
        })?;
        let callable = selected_callable_for_header(
            Some(SelectedKlibCallables::Variants(callables)),
            None,
            arena,
            declaration,
        )
        .map_err(|reason| KlibBodyDecline::new(root.signature().clone(), reason))?
        .expect("a selected root bucket contains a callable");
        self.lower_function_with_intrinsics_and_externals(
            callable, bodies, callees, intrinsics, externals,
        )
    }

    pub fn ir(&self) -> &IrFile {
        &self.ir
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

    /// Every exact dependency identity realized by this closed unit, including functions reached
    /// transitively from its selected root.
    pub fn callable_functions(
        &self,
    ) -> impl Iterator<Item = (&KlibDependencyCallable, FunId)> + '_ {
        self.functions
            .iter()
            .map(|(identity, &function)| (identity, function))
    }

    /// Ordinary classes owned by this closed unit, keyed by their authenticated constructor.
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
        let mut pending = VecDeque::new();
        let mut linker = Linker {
            functions: &mut self.functions,
            classes: &mut self.classes,
            authenticated_singleton_dispatches: &mut self.authenticated_singleton_dispatches,
            authenticated_singleton_classifiers: &mut self.authenticated_singleton_classifiers,
            pending: &mut pending,
            root: callable,
            active_module: callable.identity().module.clone(),
            sources,
        };
        let root = sources
            .bodies
            .function(callable.identity())
            .ok_or(KlibBodyDeclineReason::Unjoined)
            .and_then(|(arena, function)| {
                linker.declare(
                    &mut self.ir,
                    callable.identity(),
                    Some(callable),
                    arena,
                    function,
                )
            })
            .map_err(|reason| KlibBodyDecline::new(callable.signature().clone(), reason))?;
        while let Some(body) = linker.pending.pop_front() {
            linker.active_module.clone_from(&body.identity.module);
            let function = LoweredFunction {
                symbol: body.symbol,
                function: body.function,
            };
            let lowering = BodyLowering::new(
                body.arena,
                function,
                &body.header,
                &self.builtins,
                &mut self.ir,
                &mut linker,
            );
            let lowered = match &body.initialization {
                Some((class, plan)) => lowering.initialize_class(*class, plan),
                None => lowering.body(body.statements),
            }
            .map_err(|reason| match reason {
                KlibBodyDeclineReason::CalleeDeclined(decline) => *decline,
                reason => KlibBodyDecline::new(body.identity.declaration, reason),
            })?;
            self.ir.functions[body.function as usize].body = Some(lowered);
        }
        Ok(root)
    }
}

/// Joins the calls of the bodies being lowered to the unit's functions, declaring each callee
/// reached for the first time and queueing its body.
struct Linker<'u, 's> {
    functions: &'u mut HashMap<KlibDependencyCallable, FunId>,
    classes: &'u mut HashMap<KlibDependencyCallable, crate::ir::ClassId>,
    authenticated_singleton_dispatches: &'u mut HashMap<KlibDependencyCallable, Ty>,
    authenticated_singleton_classifiers: &'u mut HashMap<KlibDependencyCallable, TypeName>,
    pending: &'u mut VecDeque<PendingBody<'s>>,
    /// The declaration this lowering was asked for, which its own recursive calls select.
    root: KlibBodyCallable<'s>,
    /// Declaring module of the body currently being lowered. Its producer dependency graph owns
    /// the origin of every public call symbol the body serialized.
    active_module: String,
    sources: Sources<'s>,
}

impl CallLinker for Linker<'_, '_> {
    fn common_primitive(
        &self,
        signature: &KlibDeclarationSignature,
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
            .callee_origin(&self.active_module, signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing => return Ok(None),
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "ambiguous producer origin for a primitive declaration".to_owned(),
                ))
            }
        };
        let primitive = self
            .sources
            .callees
            .common_primitive(&identity, self.sources.bodies)?;
        Ok(
            if self.sources.externals.contains_key(&identity) || &identity == self.root.identity() {
                None
            } else {
                primitive
            },
        )
    }

    fn range_membership_header(
        &self,
        callee: &KlibPublicIdSignature,
    ) -> Result<Option<super::body_lowering::RangeMembershipHeader>, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let identity = match self
            .sources
            .bodies
            .callee_origin(&self.active_module, &signature)
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

    fn is_subtype(
        &self,
        parameters: &super::klib_types::KlibTypeParameters,
        sub: Ty,
        sup: Ty,
    ) -> bool {
        self.sources.callees.is_subtype(parameters, sub, sup)
    }

    fn same_type(&self, left: Ty, right: Ty) -> bool {
        self.sources.callees.same_type(left, right)
    }

    fn link_constructor(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedConstructor, KlibBodyDeclineReason> {
        let identity = self.callee_identity(callee)?;
        let (_, function) = self.sources.bodies.function(&identity).ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "a constructor call has no serialized function header".to_owned(),
            )
        })?;
        super::constructor_calls::require_constructor_kind(function)?;
        if matches!(self.sources.callees.get(&identity), Some(KlibCalleeFact::Selected(callables))
            if callables[0].kind() != crate::libraries::ExternalCallableKind::Constructor)
        {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a constructor call selected a non-constructor provider declaration".to_owned(),
            ));
        }
        let declaration = self
            .sources
            .callees
            .constructor(&identity, self.sources.bodies)?
            // Producer lookup above already proved the serialized declaration exists. Without
            // exact provider evidence this unit cannot realize construction; that is not a
            // missing declaration and must not invent a constructor handle.
            .ok_or_else(|| KlibBodyDeclineReason::UnsupportedCallee(Box::new(callee.clone())))?;
        let target = self
            .sources
            .externals
            .get(&identity)
            .filter(|external| external.is_constructor())
            .ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "an authenticated constructor has no exact constructor external handle"
                        .to_owned(),
                )
            })?
            .target();
        let selected = match self.sources.callees.get(&identity) {
            Some(KlibCalleeFact::Selected(callables)) => callables[0],
            _ => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "an authenticated constructor lost its provider declaration".to_owned(),
                ))
            }
        };
        self.materialize_class(ir, &identity, selected)?;
        Ok(LinkedConstructor {
            target,
            declaration,
        })
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

    fn singleton(&self, classifier: TypeName) -> Result<(), KlibBodyDeclineReason> {
        // The target realizes the value of an object whose representation it owns, as it does for
        // a source reference of that object. Any other object would need its class materialized.
        if self.sources.callees.target_represents(classifier) {
            return Ok(());
        }
        Err(KlibBodyDeclineReason::UnsupportedOperation(
            "an object value",
        ))
    }

    fn applied_supertype(&self, sub: Ty, owner: TypeName) -> Option<Ty> {
        // A support unit links no member of a generic class.
        (sub.kotlin_class_internal() == Some(owner)).then_some(sub)
    }

    fn link_accessor(
        &mut self,
        callee: &KlibAccessorIdSignature,
    ) -> Result<LinkedAccessor, KlibBodyDeclineReason> {
        let unsupported = || {
            KlibBodyDeclineReason::UnsupportedOperation(
                "a call through a property accessor signature",
            )
        };
        let signature = KlibDeclarationSignature::Accessor(callee.clone());
        let identity = match self
            .sources
            .bodies
            .callee_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => identity,
            KlibCalleeOrigin::Missing => return Err(unsupported()),
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(Box::new(
                    callee.property().clone(),
                )))
            }
        };
        // A complete retained export or positive target plan owns this edge. Other accessors
        // retain the explicit unsupported boundary instead of acquiring a header-only body path.
        if !self
            .sources
            .externals
            .get(&identity)
            .is_some_and(|external| external.realizes_getter())
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
        super::accessor_calls::authenticate(selected, self.sources.bodies)?.ok_or(
            KlibBodyDeclineReason::UnsupportedOperation(
                "a dependency property accessor without a getter contract",
            ),
        )
    }

    fn requires_declaration(
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
            // Compiler builtins such as `ieee754equals` deliberately have no KLIB declaration.
            // Absence therefore proves that no checked source edge selected this body callee.
            KlibCalleeOrigin::Missing => return Ok(false),
            KlibCalleeOrigin::Ambiguous => {
                return Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(Box::new(
                    callee.clone(),
                )))
            }
        };
        if &identity == self.root.identity() {
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

    fn link(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let signature = KlibDeclarationSignature::Public(callee.clone());
        let identity = self.callee_identity(callee)?;
        // The active selection decides every call, including one of a function an earlier
        // lowering already put in the unit.
        let selected = if &identity == self.root.identity() {
            Some(SelectedKlibCallables::One(self.root))
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
            });
        }
        if let Some(&emitted) = self.sources.externals.get(&identity) {
            if emitted.is_constructor() {
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
            // A member of a generic class realized outside this unit, by the target or by its
            // provider, is read at its class's type arguments, which the call binds from its
            // dispatch receiver.
            let class = match selected {
                Some(selected)
                    if matches!(purpose, HeaderPurpose::External)
                        && selected
                            .class_contract()
                            .is_some_and(|class| !class.parameters.formals.is_empty()) =>
                {
                    Some(
                        super::class_headers::authenticate(selected, self.sources.bodies)
                            .map_err(callee_declined)?,
                    )
                }
                _ => None,
            };
            let (header, class_type_parameters) = match class {
                Some(class) => {
                    super::class_headers::authenticate_receiver(
                        class.contract,
                        class.arena,
                        class.function,
                    )
                    .map_err(callee_declined)?;
                    let header = super::function_lowering::raw_function_header_with_class(
                        selected,
                        purpose,
                        class.arena,
                        class.function,
                        class.types,
                    )
                    .map_err(callee_declined)?;
                    (header, class.formals)
                }
                None => (
                    raw_function_header(selected, purpose, arena, declaration)
                        .map_err(callee_declined)?,
                    Vec::new(),
                ),
            };
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
            });
        }
        if declaration.dispatch_receiver.is_some() {
            // A member call passes its dispatch receiver apart from its arguments and dispatches
            // virtually unless the member is final; lowering models neither yet.
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a call of a member function",
            ));
        }
        let selected = selected_callable_for_header(selected, None, arena, declaration)
            .map_err(callee_declined)?;
        let function = match self.functions.get(&identity) {
            Some(function) => {
                // The function was declared under another lowering's selection, or from its
                // header alone; this lowering's selection must describe it too.
                if let Some(selected) = selected {
                    function_header(Some(selected), arena, declaration).map_err(callee_declined)?;
                }
                *function
            }
            None => self
                .declare(ir, &identity, selected, arena, declaration)
                .map_err(callee_declined)?,
        };
        let declaration = &ir.functions[function as usize];
        Ok(LinkedCallee {
            callee: Callee::Local(function),
            receiver: None,
            params: declaration.params.clone(),
            ret: declaration.ret,
            context_count: ir.fn_context_counts.get(&function).copied().unwrap_or(0),
            singleton_dispatch: None,
            type_parameters: selected
                .and_then(|selected| selected.generic_signature())
                .map_or_else(Vec::new, |signature| signature.formals.clone()),
            type_parameter_bounds: selected
                .and_then(|selected| selected.generic_signature())
                .map(|signature| signature.formal_bounds.clone()),
            class_type_parameters: Vec::new(),
        })
    }
}

impl<'s> Linker<'_, 's> {
    /// The type parameters of the generic class declaring `selected`, which a call of the member
    /// binds from its dispatch receiver; none for a top-level function or a non-generic class.
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
            .callee_origin(&self.active_module, &signature)
        {
            KlibCalleeOrigin::Exact(identity) => Ok(identity),
            KlibCalleeOrigin::Ambiguous => Err(KlibBodyDeclineReason::AmbiguousCalleeOrigin(
                Box::new(callee.clone()),
            )),
            KlibCalleeOrigin::Missing if callee.declaration().segments().len() > 1 => {
                // A member no library serializes is, in a complete library set, a fake override:
                // its real declaration is reached through serialized class declarations, which
                // lowering does not follow yet.
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
        let statements = match &function.body {
            None => return Err(KlibBodyDeclineReason::NoBody),
            Some(KlibIrBody::Synthetic(_)) => return Err(KlibBodyDeclineReason::SyntheticBody),
            Some(KlibIrBody::Block(statements)) => statements,
        };
        let header = function_header(selected, arena, function)?;
        let declared = declare_function(ir, &header);
        self.functions.insert(identity.clone(), declared);
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
        });
        Ok(declared)
    }
}

/// Select the exact checked edge variant that agrees with one serialized declaration. Several
/// singleton objects may inherit the same declaration identity, but only the variant whose
/// semantic receiver matches this body (and an already-emitted contract, when present) can
/// authenticate the call. Declaration-shape conflicts remain ambiguous in `KlibCalleeFacts`.
#[derive(Clone, Copy)]
enum SelectedKlibCallables<'callable, 'facts> {
    One(KlibBodyCallable<'callable>),
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
    // A member of a generic class binds its class's type parameters from the class header.
    let header = |candidate: KlibBodyCallable<'callable>| match class_bodies.filter(|_| {
        candidate
            .class_contract()
            .is_some_and(|class| !class.parameters.formals.is_empty())
    }) {
        Some(bodies) => super::class_headers::authenticate(candidate, bodies).and_then(|class| {
            super::function_lowering::raw_function_header_with_class(
                Some(candidate),
                purpose,
                class.arena,
                class.function,
                class.types,
            )
        }),
        None => raw_function_header(Some(candidate), purpose, arena, function),
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
        SelectedKlibCallables::One(candidate) => {
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
        unit.classes
            .retain(|_, class| (*class as usize) < unit.ir.classes.len());
        let functions = &unit.functions;
        unit.authenticated_singleton_dispatches
            .retain(|identity, _| functions.contains_key(identity));
        unit.authenticated_singleton_classifiers
            .retain(|identity, _| functions.contains_key(identity));
    }
}
