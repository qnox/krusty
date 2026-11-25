//! Exact ordinary dependency declarations referenced by structurally emitted expression bodies.

use crate::backend::CheckedIrFile;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{Callee, IrCheckedOperation, IrExpr, IrFile};
use crate::klib_libraries::{KlibDeclarationBodies, KlibIntrinsicFacts};
use crate::libraries::{
    ExternalCallableKind, ExternalCallableRealization, KlibBodyCallable, KlibDependencyCallable,
    SemanticPlatform,
};
use std::collections::VecDeque;

/// The exact external callable declarations reached from emitted expression roots. The frozen
/// callable inventory includes expression-arena nodes left behind by earlier rewrites; those nodes
/// are declaration evidence, not executable uses, and must not cause intrinsic support emission.
pub(super) fn emitted_dependency_targets(
    ir: &IrFile,
    mut property_accessor: impl FnMut(
        crate::fir::ExternalPropertyId,
        bool,
    ) -> Result<Option<crate::fir::ExternalCallableId>, String>,
) -> Result<Vec<(crate::ir::ExprId, crate::fir::ExternalCallableId, bool)>, String> {
    let mut targets = Vec::new();
    for expression in crate::ir::emitted_body_roots(ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(ir, root))
    {
        match ir.expr(expression) {
            IrExpr::Call { callee, .. } => {
                if let Callee::External { target, .. } = &**callee {
                    targets.push((expression, *target, false));
                }
            }
            IrExpr::New {
                external_target: Some(target),
                ..
            } => targets.push((expression, *target, true)),
            IrExpr::Checked(operation) => match &**operation {
                IrCheckedOperation::ExternalPropertyRead { target, .. } => {
                    if let Some(callable) = property_accessor(*target, false)? {
                        targets.push((expression, callable, false));
                    }
                }
                IrCheckedOperation::ExternalPropertyWrite { target, .. } => {
                    if let Some(callable) = property_accessor(*target, true)? {
                        targets.push((expression, callable, false));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(targets)
}

/// The exact external callables `source` emits a use of, its property reads and writes through
/// their selected accessors.
fn emitted_file_targets(
    source: &CheckedIrFile<'_>,
) -> Result<Vec<(crate::ir::ExprId, crate::fir::ExternalCallableId, bool)>, String> {
    emitted_dependency_targets(&source.ir, |property, setter| {
        let property = source
            .callables
            .property(property)
            .ok_or("an emitted dependency property has no frozen fact")?;
        if setter {
            property
                .setter
                .map(Some)
                .ok_or("an emitted dependency property write has no selected setter".to_owned())
        } else {
            Ok(Some(property.getter))
        }
    })
}

/// The dependency declarations `source` emits a use of.
pub(super) fn emitted_dependency_callables(
    source: &CheckedIrFile<'_>,
) -> Result<HashSet<KlibDependencyCallable>, String> {
    Ok(emitted_file_targets(source)?
        .into_iter()
        .filter_map(|(_, target, _)| {
            source
                .callables
                .callable(target)
                .and_then(|callable| callable.dependency_callable.clone())
        })
        .collect())
}

/// The dependency interface members the classes of `ir` inherit with a body and do not override.
/// A target that dispatches through interface records calls each of them from its class's record,
/// so they are emitted uses even though no expression of the file calls them.
pub(super) fn emitted_inherited_default_targets(
    ir: &IrFile,
) -> impl Iterator<Item = crate::fir::ExternalCallableId> + '_ {
    ir.classes
        .iter()
        .filter(|class| !class.is_interface)
        .flat_map(|class| {
            ir.inherited_defaults
                .get(&class.fq_name_id())
                .into_iter()
                .flatten()
        })
        .filter_map(|default| match default.body {
            crate::fir::InheritedDefaultBody::DependencyInterfaceMethod(declaration)
            | crate::fir::InheritedDefaultBody::DependencyHolder(declaration) => Some(declaration),
            crate::fir::InheritedDefaultBody::Module
            | crate::fir::InheritedDefaultBody::JavaDefaultMethod => None,
        })
}

/// The declarations a unit's closure freezes.
pub(super) struct FrozenClosure {
    pub(super) declarations: Vec<(crate::fir::ExternalCallableId, ExternalCallableRealization)>,
    /// Edges that authorize a provider call of a member realized outside the unit. Function edges
    /// are the ones the checker froze. An accessor has no function-hierarchy edge; the closure's
    /// empty edge is the authorization the provider call consumes.
    pub(super) authorized_member_edges:
        HashMap<KlibDependencyCallable, crate::klib_lowering::CallEdge>,
    /// The library objects whose values the closure's bodies read, which the unit materializes.
    pub(super) read_classes: Vec<crate::libraries::KlibBodyClassContract>,
    /// Members of a materialized class or object that dispatch reaches, whose bodies the unit
    /// emits although no walked body calls them directly.
    pub(super) dispatched: Vec<KlibDependencyCallable>,
    /// The classes declaring a private member a class nested in them calls, which the unit
    /// materializes to emit that member.
    pub(super) private_member_classes: Vec<crate::libraries::KlibBodyClassContract>,
}

/// Where a unit's closure starts: the callables the emitted source selects, and the library
/// objects it reads as values.
pub(super) struct ClosureRoots<'r, 'a> {
    pub(super) callables: &'r [KlibBodyCallable<'a>],
    pub(super) objects: &'r [crate::libraries::KlibBodyClassContract],
}

/// Freeze declaration contracts along the exact producer graph before ordinary body lowering.
/// A body-bearing callee cannot regain the legacy header-only path merely because source FIR did
/// not also select it. Intrinsic/bodyless leaves retain their separately authenticated boundary.
pub(super) fn declarations(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    intrinsics: &KlibIntrinsicFacts,
    roots: ClosureRoots<'_, '_>,
    externals: &HashMap<KlibDependencyCallable, super::EmittedDependencyCallable>,
    capabilities: &super::DependencyLeafCapabilities<'_>,
    requires_ordinary: bool,
) -> Result<FrozenClosure, String> {
    // Each callee, with the materialized class whose member's body calls it.
    let root_objects = roots.objects;
    let mut visited = HashSet::default();
    let mut authorized_member_edges = HashMap::default();
    let mut state = ClosureState {
        pending: roots
            .callables
            .iter()
            .map(|root| (root.identity().clone(), None))
            .collect(),
        declarations: Vec::new(),
        declared: HashSet::default(),
        objects: HashSet::default(),
        read_classes: root_objects.to_vec(),
        dispatched: HashSet::default(),
        private_member_classes: Vec::new(),
        emitted_defaults: roots
            .callables
            .iter()
            .filter(|root| {
                root.kind() == ExternalCallableKind::Member
                    && root
                        .class_contract()
                        .is_some_and(crate::libraries::KlibBodyClassContract::emits_default_members)
            })
            .map(|root| root.identity().clone())
            .collect(),
        capabilities,
    };
    // An object the emitted source reads as a value is materialized as one a body reads.
    for class in root_objects {
        walk_declared_instances(
            platform,
            bodies,
            capabilities.callees,
            class.clone(),
            &mut state,
        )?;
    }
    while let Some((identity, member_class)) = state.pending.pop_front() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        // A private member of a materialized class has no provider declaration; the member calling it
        // authenticates its class, and it is emitted as that member is.
        if let Some(class) = member_class
            .filter(|_| {
                platform
                    .external_callable_by_dependency(&identity)
                    .is_none()
                    && !externals.contains_key(&identity)
            })
            .and_then(|caller| {
                let declaring = private_member_class(platform, bodies, &identity, caller.clone())?;
                if declaring.classifier != caller.classifier
                    && !state
                        .private_member_classes
                        .iter()
                        .any(|known| known.classifier == declaring.classifier)
                {
                    state.private_member_classes.push(declaring.clone());
                }
                Some(declaring)
            })
        {
            let emitted = crate::klib_lowering::emitted_body(bodies, &identity);
            state.pending.extend(
                emitted
                    .callables
                    .into_iter()
                    .map(|callee| (callee, Some(class.clone()))),
            );
            read_values(
                platform,
                bodies,
                capabilities.callees,
                &emitted.objects,
                &mut state,
            )?;
            continue;
        }
        // A top-level function private to its file, or a member of a class private to its
        // library, has no provider declaration either; only a body of its own module calls it, and
        // it is emitted with that body. One an earlier unit already emitted is that unit's export
        // instead, an exact external edge which keeps its declaration authentication below.
        if bodies.is_library_private(&identity) && !externals.contains_key(&identity) {
            let emitted = crate::klib_lowering::emitted_body(bodies, &identity);
            state
                .pending
                .extend(emitted.callables.into_iter().map(|callee| (callee, None)));
            read_values(
                platform,
                bodies,
                capabilities.callees,
                &emitted.objects,
                &mut state,
            )?;
            continue;
        }
        // Positive common-lowering capability, never a missing-provider fallback. Earlier emitted
        // exports remain exact external edges and retain their declaration authentication.
        let dispatched_member = state.dispatched.contains(&identity);
        if !externals.contains_key(&identity)
            && !dispatched_member
            && capabilities.folds_common_builtin(&identity)
        {
            continue;
        }
        // Intrinsic support already admits body-bearing raw headers without a source/provider
        // selection. Optional enrichment does not invent a primitive on a miss. Ordinary bodies,
        // retained exports, and target-intrinsic/bodyless leaves still require their provider.
        let raw_header_capability = !requires_ordinary
            && !externals.contains_key(&identity)
            && intrinsics.get(&identity).is_none()
            && bodies.function_has_body(&identity) == Some(true);
        let known_target = capabilities
            .callees
            .target_intrinsic(&identity, bodies, capabilities.target_realizes_intrinsic)
            .map_err(|reason| format!("an invalid target declaration: {reason:?}"))?;
        let (declaration_target, realization) =
            match platform.external_callable_by_dependency(&identity) {
                Some(declaration) => declaration,
                None if raw_header_capability && known_target.is_none() => {
                    let emitted = crate::klib_lowering::emitted_body(bodies, &identity);
                    state
                        .pending
                        .extend(emitted.callables.into_iter().map(|callee| (callee, None)));
                    read_values(
                        platform,
                        bodies,
                        capabilities.callees,
                        &emitted.objects,
                        &mut state,
                    )?;
                    continue;
                }
                None => {
                    return Err(format!(
                    "a KLIB dependency has no required exact provider declaration: {identity:?}"
                ))
                }
            };
        if realization.dependency_callable.as_ref() != Some(&identity) {
            return Err("an ordinary KLIB body provider changed its exact identity".to_owned());
        }
        let selected = realization
            .klib_body_callable()
            .ok_or("a dependency provider has no KLIB body contract")?;
        // An enum class's implicit `values`, `valueOf` or `entries` is realized by the common
        // enum realization of its class, which the unit builds from the class's initialization.
        if let Some(class) = implicit_enum_member_class(platform, selected) {
            if !(state.capabilities.target_represents_classifier)(class.classifier)
                && !state.capabilities.unit.reads_realized(class.classifier)
            {
                read_class(platform, bodies, capabilities.callees, class, &mut state)?;
            }
            state.declarations.push((declaration_target, realization));
            continue;
        }
        let superclasses = superclass_realizations(platform, bodies, &identity);
        let facts = with_realized_declarations(
            capabilities.callees.with_declaration(selected),
            &superclasses,
        );
        // A continuation interception expands to its coroutine class's own interception, which
        // the expansion calls in its stead: the closure reaches that member, never the
        // intrinsic's excluded body. The intrinsic's declaration is what selects the expansion.
        if let Some(member) = crate::klib_lowering::expansion_member(selected, bodies)
            .map_err(|reason| format!("an invalid continuation interception: {reason:?}"))?
        {
            state.pending.push_back((member, None));
            state.declarations.push((declaration_target, realization));
            continue;
        }
        // A counted loop over a collection's or text's indices reads the receiver's size, which
        // the loop's lowering calls in its place.
        if let Some(member) = crate::klib_lowering::indices_bound_member(selected, bodies)
            .map_err(|reason| format!("an invalid indices bound: {reason:?}"))?
        {
            state.pending.push_back((member, None));
        }
        let retained = externals
            .get(&identity)
            .and_then(|external| external.constructor_initializer_target());
        if let Some(target) = retained {
            if target != declaration_target {
                return Err("an emitted initializer changed its rebound provider handle".into());
            }
            if !facts
                .authenticates_retained_constructor(&identity, bodies)
                .map_err(|reason| {
                    format!("an invalid retained constructor declaration: {reason:?}")
                })?
            {
                return Err("an emitted initializer lost its constructor declaration".into());
            }
            state.declarations.push((declaration_target, realization));
            continue;
        }
        let constructor = facts
            .authenticates_constructor(&identity, bodies)
            .map_err(|reason| format!("an invalid constructor declaration: {reason:?}"))?;
        if constructor {
            if let Some(callees) = facts
                .constructor_initialization_callees(&identity, bodies)
                .map_err(|reason| format!("an invalid class initialization: {reason:?}"))?
            {
                // The initialization reads the class's private members as its members do.
                let member_class = selected
                    .class_contract()
                    .filter(|class| class.emits_members())
                    .cloned();
                state.pending.extend(
                    callees
                        .into_iter()
                        .map(|callee| (callee, member_class.clone())),
                );
                // And the objects it reads are materialized with the class.
                let objects = facts
                    .constructor_initialization_objects(&identity, bodies)
                    .map_err(|reason| format!("an invalid class initialization: {reason:?}"))?;
                read_values(platform, bodies, &facts, &objects, &mut state)?;
            }
            let dispatched = facts
                .constructor_dispatched_members(&identity, bodies)
                .map_err(|reason| format!("an invalid class member: {reason:?}"))?;
            // Each is emitted with the class: a member the class inherits implements interface
            // declarations only dispatch reaches, and no body calls it.
            let member_class = selected
                .class_contract()
                .filter(|class| class.emits_members())
                .cloned();
            state.pending.extend(
                dispatched
                    .iter()
                    .map(|member| (member.clone(), member_class.clone())),
            );
            state.dispatched.extend(dispatched);
            // The declarations the class's initialization names without emitting them.
            for declaration in facts
                .constructor_declaration_evidence(&identity, bodies)
                .map_err(|reason| format!("an invalid class declaration: {reason:?}"))?
            {
                if state.declared.insert(declaration.clone()) {
                    state.declarations.push(
                        platform
                            .external_callable_by_dependency(&declaration)
                            .ok_or_else(|| {
                                format!(
                                    "a KLIB declaration a class names has no provider: {declaration:?}"
                                )
                            })?,
                    );
                }
            }
        }
        // So does a construction of one, whose box the unit's entry for the constructor builds.
        if constructor
            && selected
                .class_contract()
                .is_some_and(crate::libraries::KlibBodyClassContract::has_unit_box)
        {
            walk_value_class_overrides(platform, bodies, selected, &mut state)?;
        }
        let primitive = facts
            .common_primitive(&identity, bodies)
            .map_err(|reason| format!("an invalid common primitive declaration: {reason:?}"))?;
        let target = facts
            .target_intrinsic(&identity, bodies, capabilities.target_realizes_intrinsic)
            .map_err(|reason| format!("an invalid target declaration: {reason:?}"))?;
        let getter = !externals.contains_key(&identity)
            && primitive.is_none()
            && target.is_none()
            && intrinsics.get(&identity).is_none()
            && facts
                .target_getter(&identity, bodies, capabilities.target_realizes_getter)
                .map_err(|reason| format!("an invalid target getter declaration: {reason:?}"))?;
        let member = facts
            .target_member(&identity, capabilities.target_represents_classifier)
            .map_err(|reason| format!("an invalid target member declaration: {reason:?}"))?;
        // A member of a class the target lays out, or of one no support unit materializes, is a
        // leaf: support bodies call it through its declaration, as source calls do, and the
        // target realizes it. Every target realizes one of `kotlin.Any`'s members, or a
        // declaration overriding one, from the receiver's own representation, as it does for a
        // source call. A target which realizes no other declaration outside its emitted bodies
        // has no other such leaf: body lowering declines a call of one, as it declines any
        // unmodelled form.
        let emitted_default = state.emitted_defaults.contains(&identity);
        let outside_edge = if member
            || (!emitted_default && is_outside_units(platform, bodies, &facts, selected)?)
        {
            Some(member_edge(&facts, selected)?)
        } else {
            None
        };
        let external_member = outside_edge.as_ref().is_some_and(|edge| {
            capabilities.target_realizes_bodyless || edge.realizes_any_member()
        });
        // A call of a member realized outside the unit carries the facts the member hierarchy
        // decorates a source call of it with; the edge is what authorizes the call.
        if let Some(edge) = outside_edge.filter(|_| external_member) {
            authorized_member_edges.insert(identity.clone(), edge);
        }
        let emits_body = !externals.contains_key(&identity)
            && !member
            && !external_member
            && !constructor
            && (dispatched_member || primitive.is_none())
            && target.is_none()
            && !getter
            && intrinsics.get(&identity).is_none()
            && bodies.function_has_body(&identity) == Some(true);
        if requires_ordinary
            && emits_body
            && (!matches!(
                realization.kind,
                ExternalCallableKind::TopLevel
                    | ExternalCallableKind::Extension
                    | ExternalCallableKind::Member
            ) || !realization
                .klib_body_declaration_modifiers
                .is_some_and(|modifiers| {
                    modifiers.permits_ordinary_body() || modifiers.permits_expanded_body()
                }))
        {
            return Err(format!("a transitive KLIB body has no supported ordinary declaration contract: {identity:?}"));
        }
        if emits_body {
            // A member of an ordinary class is called on an instance one of the constructors
            // that initialize the class created, which materializes the class.
            if let Some(class) = selected.class_contract().filter(|class| {
                !class.is_value
                    && !class.declares_instances()
                    && selected.kind() == ExternalCallableKind::Member
            }) {
                state.pending.extend(
                    ordinary_class_constructors(platform, bodies, &facts, class)?
                        .into_iter()
                        .map(|constructor| (constructor, None)),
                );
            }
            // A member of a value class materializes the class, whose boxed instances dispatch to
            // its overrides.
            if selected
                .class_contract()
                .is_some_and(crate::libraries::KlibBodyClassContract::has_unit_box)
                && selected.kind() == ExternalCallableKind::Member
            {
                walk_value_class_overrides(platform, bodies, selected, &mut state)?;
            }
            // A member of an object reads the instance the object's own initialization created,
            // which materializes the object once with every member dispatch reaches. So does a
            // body reading an object's value, unless the target represents that object itself.
            if let Some(class) = member_object(platform, &identity) {
                walk_declared_instances(platform, bodies, &facts, class, &mut state)?;
            }
            let emitted = crate::klib_lowering::emitted_body(bodies, &identity);
            read_values(platform, bodies, &facts, &emitted.objects, &mut state)?;
            let member_class = selected
                .class_contract()
                .filter(|class| class.emits_members())
                .cloned();
            state.pending.extend(
                emitted
                    .callables
                    .into_iter()
                    .map(|callee| (callee, member_class.clone())),
            );
        }
        state.declarations.push((declaration_target, realization));
    }
    Ok(FrozenClosure {
        declarations: state.declarations,
        authorized_member_edges,
        read_classes: state.read_classes,
        dispatched: state.dispatched.into_iter().collect(),
        private_member_classes: state.private_member_classes,
    })
}

pub(super) struct OrdinaryRootCapabilities<'a> {
    pub(super) realizes_intrinsic: &'a dyn Fn(crate::libraries::CompilerIntrinsic) -> bool,
    pub(super) realizes_getter: &'a dyn Fn(crate::backend::DependencyGetterSignature<'_>) -> bool,
    pub(super) represents_classifier: &'a dyn Fn(crate::types::TypeName) -> bool,
    pub(super) retains_initializer: &'a dyn Fn(KlibBodyCallable<'_>) -> Result<bool, &'static str>,
}

/// What the closure accumulates while it walks the producer graph.
struct ClosureState<'c, 'u> {
    /// Each callee, with the materialized class whose member's body calls it.
    pending: VecDeque<(
        KlibDependencyCallable,
        Option<crate::libraries::KlibBodyClassContract>,
    )>,
    declarations: Vec<(crate::fir::ExternalCallableId, ExternalCallableRealization)>,
    /// Declaration-only evidence already pushed, which the closure never walks.
    declared: HashSet<KlibDependencyCallable>,
    /// The objects whose initialization the closure already walks.
    objects: HashSet<crate::types::TypeName>,
    /// The library objects whose values the walked bodies read, which the unit materializes.
    read_classes: Vec<crate::libraries::KlibBodyClassContract>,
    /// Members of a materialized class that dispatch reaches: their bodies are emitted even where
    /// a call of one folds to a common operation, as a virtual call runs the body.
    dispatched: HashSet<KlibDependencyCallable>,
    /// The classes declaring a private member a class nested in them calls.
    private_member_classes: Vec<crate::libraries::KlibBodyClassContract>,
    /// Interface default bodies a class of the file inherits, emitted for its dispatch record
    /// though no support unit materializes the interface.
    emitted_defaults: HashSet<KlibDependencyCallable>,
    capabilities: &'c super::DependencyLeafCapabilities<'u>,
}

/// Record the library objects `objects` a walked body or initialization reads as values, and walk
/// each one's initialization. An object the target represents itself, or one an earlier unit
/// materialized and exports the instance of, is read as it is and needs no walk.
fn read_values(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    facts: &crate::klib_lowering::KlibCalleeFacts<'_>,
    objects: &[crate::metadata::id_signature::KlibPublicIdSignature],
    state: &mut ClosureState<'_, '_>,
) -> Result<(), String> {
    for classifier in objects
        .iter()
        .filter_map(crate::klib_lowering::object_classifier)
    {
        if (state.capabilities.target_represents_classifier)(classifier)
            || state.capabilities.unit.reads_realized(classifier)
        {
            continue;
        }
        let Some(class) = platform
            .klib_body_class_contract(classifier)
            .filter(|class| class.declares_instances() && class.emits_members())
        else {
            continue;
        };
        read_class(platform, bodies, facts, class, state)?;
    }
    Ok(())
}

/// Record that a walked body reads an instance of `class`, an object or an enum class, and walk
/// its initialization. A class whose initialization no unit admits is realized where its own
/// module exports its instances, as a call of its member is.
fn read_class(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    facts: &crate::klib_lowering::KlibCalleeFacts<'_>,
    class: crate::libraries::KlibBodyClassContract,
    state: &mut ClosureState<'_, '_>,
) -> Result<(), String> {
    if declared_instances_initialization(platform, bodies, facts, &class)?.is_none() {
        return Ok(());
    }
    if !state
        .read_classes
        .iter()
        .any(|read| read.classifier == class.classifier)
    {
        state.read_classes.push(class.clone());
    }
    walk_declared_instances(platform, bodies, facts, class, state)
}

/// Walk the initialization of the object `class` once: the object is materialized with every
/// member dispatch reaches, and the declarations its members override are named.
/// The materialized class declaring `identity`, a private member called from a member of `caller`.
/// Kotlin makes a private member visible anywhere inside its declaring class, and a private member
/// of a companion object anywhere inside the class containing that companion, so the caller may be
/// the declaring class, a class nested in it, or the class whose companion declares the member.
fn private_member_class(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
    caller: crate::libraries::KlibBodyClassContract,
) -> Option<crate::libraries::KlibBodyClassContract> {
    if crate::klib_lowering::is_private_class_member(bodies, identity, &caller) {
        return Some(caller);
    }
    let crate::klib_libraries::LocatedMember::One { class, .. } = bodies.member_owner(identity)
    else {
        return None;
    };
    let crate::metadata::klib_ir::KlibIrSignature::Public(signature) = &class.base.symbol.signature
    else {
        return None;
    };
    let declaring = crate::klib_lowering::object_classifier(signature)?;
    let scope = if crate::metadata::semantic::class_is_companion(class.base.flags) {
        declaring.nested_owner()?
    } else {
        declaring
    };
    std::iter::successors(Some(caller.classifier), |class| class.nested_owner())
        .any(|class| class == scope)
        .then(|| platform.klib_body_class_contract(declaring))
        .flatten()
        .filter(|class| crate::klib_lowering::is_private_class_member(bodies, identity, class))
}

fn walk_declared_instances(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    facts: &crate::klib_lowering::KlibCalleeFacts<'_>,
    class: crate::libraries::KlibBodyClassContract,
    walk: &mut ClosureState<'_, '_>,
) -> Result<(), String> {
    if !walk.objects.insert(class.classifier) {
        return Ok(());
    }
    let Some(initialization) = declared_instances_initialization(platform, bodies, facts, &class)?
    else {
        return Ok(());
    };
    walk.pending.extend(
        initialization
            .callees
            .into_iter()
            .map(|callee| (callee, Some(class.clone()))),
    );
    walk.dispatched.extend(
        facts
            .declared_instances_dispatched_members(&class, bodies)
            .map_err(|reason| format!("an invalid object member: {reason:?}"))?,
    );
    for overridden in facts
        .declared_instances_overridden_declarations(&class, bodies)
        .map_err(|reason| format!("an invalid object override: {reason:?}"))?
    {
        if walk.declared.insert(overridden.clone()) {
            walk.declarations.push(
                platform
                    .external_callable_by_dependency(&overridden)
                    .ok_or_else(|| {
                        format!("an overridden KLIB declaration has no provider: {overridden:?}")
                    })?,
            );
        }
    }
    // The objects the initialization reads are materialized with this one.
    read_values(platform, bodies, facts, &initialization.objects, walk)
}

/// The library objects the emitted source reads as values, which a support unit materializes:
/// every object a KLIB serializes whose representation the target does not own and whose
/// initialization a unit admits, as for an object whose member the source calls. Any other object
/// is realized where its own module exports its instance.
pub(super) fn ordinary_root_objects(
    platform: &dyn SemanticPlatform,
    source: &CheckedIrFile<'_>,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    target_represents_classifier: &dyn Fn(crate::types::TypeName) -> bool,
) -> Result<Vec<crate::libraries::KlibBodyClassContract>, String> {
    let mut objects: Vec<crate::libraries::KlibBodyClassContract> = Vec::new();
    for expression in crate::ir::emitted_body_roots(&source.ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(&source.ir, root))
    {
        let IrExpr::SingletonValue { classifier } = source.ir.expr(expression) else {
            continue;
        };
        let classifier = *classifier;
        if source.ir.class_id_by_name(classifier).is_some()
            || target_represents_classifier(classifier)
            || objects.iter().any(|object| object.classifier == classifier)
        {
            continue;
        }
        let Some(class) = platform
            .klib_body_class_contract(classifier)
            .filter(|class| class.declares_instances() && class.emits_members())
        else {
            continue;
        };
        if declared_instances_initialization(platform, bodies, callees, &class)?.is_some() {
            objects.push(class);
        }
    }
    Ok(objects)
}

pub(super) fn ordinary_roots<'a>(
    platform: &dyn SemanticPlatform,
    source: &'a CheckedIrFile<'_>,
    bodies: &KlibDeclarationBodies,
    intrinsics: &KlibIntrinsicFacts,
    callees: &crate::klib_lowering::KlibCalleeFacts<'a>,
    capabilities: &OrdinaryRootCapabilities<'_>,
) -> Result<Vec<KlibBodyCallable<'a>>, String> {
    let target_realizes_intrinsic = capabilities.realizes_intrinsic;
    let target_represents_classifier = capabilities.represents_classifier;
    let mut roots = Vec::new();
    let mut seen = HashSet::default();
    for (expression, target, construction) in emitted_file_targets(source)? {
        let callable = source
            .callables
            .callable(target)
            .ok_or("an emitted dependency callable has no frozen fact")?;
        let Some(selected) = source.callables.klib_body_callable_at(expression, target) else {
            continue;
        };
        if construction != (callable.kind == ExternalCallableKind::Constructor) {
            return Err(
                "an emitted dependency construction disagrees with its selected callable role"
                    .to_owned(),
            );
        }
        if construction {
            // A target can already provide a constructor whose KLIB publishes metadata only.
            // Only an executable serialized declaration asks this phase to realize its body.
            if bodies.function_has_body(selected.identity()) != Some(true) {
                continue;
            }
            // A value class's box is materialized by the construction itself. Otherwise the
            // constructor's delegation chain is admitted with the declarations it reaches, as the
            // closure admits it.
            let superclasses = superclass_realizations(platform, bodies, selected.identity());
            if (selected
                .class_contract()
                .is_some_and(crate::libraries::KlibBodyClassContract::has_unit_box)
                || with_realized_declarations(callees.clone(), &superclasses)
                    .constructor_initialization_callees(selected.identity(), bodies)
                    .map_err(|reason| format!("an invalid constructor root: {reason:?}"))?
                    .is_some())
                && seen.insert(selected.identity())
            {
                roots.push(selected);
            }
            continue;
        }
        // Virtual instance dispatch is a different emission capability. This category accepts
        // declaration-owned top-level and extension frames, and the members of a class the unit
        // materializes, which dispatch statically.
        if !callable.is_top_level()
            && !is_materialized_class_member(platform, bodies, callees, selected)?
        {
            continue;
        }
        // A source call of an overridable member of an open or abstract class dispatches on an
        // instance of any subclass. A final member has the one body, but its instance may still be
        // one the target lays out itself (an array's `IntIterator`), which only a class the target
        // represents has; every instance of any other class is built by a constructor of the
        // program.
        if !callable.is_top_level()
            && selected.class_contract().is_some_and(|class| {
                !class.is_value
                    && class.modality != crate::metadata::semantic::KotlinModality::Final
                    && (target_represents_classifier(class.classifier)
                        || callable
                            .klib_body_declaration_modifiers
                            .is_none_or(|modifiers| modifiers.is_overridable()))
            })
        {
            continue;
        }
        // A member of a classifier the target represents itself is the target's to realize.
        if callees
            .target_member(selected.identity(), target_represents_classifier)
            .map_err(|reason| format!("an invalid target member declaration: {reason:?}"))?
        {
            continue;
        }
        if callees
            .target_intrinsic(selected.identity(), bodies, target_realizes_intrinsic)
            .map_err(|reason| format!("an invalid target declaration: {reason:?}"))?
            .is_some()
        {
            continue;
        }
        // A getter the target answers itself (an array's `storage`) is realized at each read,
        // as the closure leaves it, never lowered from its body.
        if callees
            .target_getter(selected.identity(), bodies, capabilities.realizes_getter)
            .map_err(|reason| format!("an invalid target getter declaration: {reason:?}"))?
        {
            continue;
        }
        let Some(modifiers) = callable.klib_body_declaration_modifiers else {
            continue;
        };
        if !modifiers.permits_ordinary_body()
            || intrinsics.get(selected.identity()).is_some()
            || bodies.function_has_body(selected.identity()) != Some(true)
        {
            continue;
        }
        // The complete selected/raw header is authenticated by transactional unit lowering.
        // A malformed known ordinary body is an obligation, not a failed eligibility probe.
        if seen.insert(selected.identity()) {
            roots.push(selected);
        }
    }
    // A class of this file extending a dependency class initializes each instance with the
    // dependency constructor it delegates to, so that constructor materializes its class just as
    // a construction does.
    let mut delegations = source
        .ir
        .external_super_constructors
        .values()
        .chain(source.ir.external_secondary_super_constructors.values())
        .map(|target| target.declaration)
        .collect::<Vec<_>>();
    delegations.sort_unstable_by_key(|target| target.raw());
    delegations.dedup();
    for target in delegations {
        let Some(selected) = source
            .callables
            .callable(target)
            .ok_or("a dependency superclass constructor has no frozen fact")?
            .klib_body_callable()
        else {
            continue;
        };
        if (capabilities.retains_initializer)(selected)? {
            if !callees
                .authenticates_retained_constructor(selected.identity(), bodies)
                .map_err(|reason| {
                    format!("an invalid retained superclass initializer: {reason:?}")
                })?
            {
                return Err("a retained superclass initializer without its authenticated constructor header".into());
            }
            continue;
        }
        if bodies.function_has_body(selected.identity()) != Some(true) {
            continue;
        }
        if callees
            .constructor_initialization_callees(selected.identity(), bodies)
            .map_err(|reason| format!("an invalid superclass constructor root: {reason:?}"))?
            .is_some()
            && seen.insert(selected.identity())
        {
            roots.push(selected);
        }
    }
    // A class of this file inherits a dependency interface's default and does not override it, so
    // its dispatch record calls that default's body. The body is an emitted use of the class, not
    // of an expression, and dispatches statically from the record.
    let mut defaults = emitted_inherited_default_targets(&source.ir).collect::<Vec<_>>();
    defaults.sort_unstable_by_key(|target| target.raw());
    defaults.dedup();
    for target in defaults {
        let Some(selected) = source
            .callables
            .callable(target)
            .ok_or("an inherited dependency default has no frozen fact")?
            .klib_body_callable()
        else {
            continue;
        };
        if intrinsics.get(selected.identity()).is_none()
            && bodies.function_has_body(selected.identity()) == Some(true)
            && seen.insert(selected.identity())
        {
            roots.push(selected);
        }
    }
    Ok(roots)
}

/// Whether `callable` is a member of a class which a support unit materializes and emits the
/// member as a member of. A value class is materialized by its members; an ordinary class only by
/// the admitted initialization of its primary constructor, the same plan lowering executes. A
/// builtin type of the common type model is neither.
fn is_materialized_class_member(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    callable: KlibBodyCallable<'_>,
) -> Result<bool, String> {
    if callable.kind() != ExternalCallableKind::Member {
        return Ok(false);
    }
    if crate::klib_lowering::is_receiver_member(callable, bodies, callees) {
        // Every target realizes one of `kotlin.Any`'s members from the receiver's own
        // representation, a builtin value included, so such a member stays the target's.
        return Ok(!member_edge(callees, callable)?.realizes_any_member());
    }
    let Some(class) = callable
        .class_contract()
        .filter(|class| class.emits_members())
    else {
        return Ok(false);
    };
    if class.is_value {
        return Ok(true);
    }
    if class.declares_instances() {
        return Ok(declared_instances_initialization(platform, bodies, callees, class)?.is_some());
    }
    Ok(!ordinary_class_constructors(platform, bodies, callees, class)?.is_empty())
}

/// The initialization edges of the object `class`, when its admitted initialization materializes
/// it. Its private primary constructor has no provider declaration: the object's own members
/// carry its class, and only its declaration creates its instance.
fn declared_instances_initialization(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    class: &crate::libraries::KlibBodyClassContract,
) -> Result<Option<DeclaredInstancesInitialization>, String> {
    let Some(primary) = class.primary_constructor.clone() else {
        return Ok(None);
    };
    let identity = KlibDependencyCallable {
        module: class.module.clone(),
        declaration: crate::libraries::KlibDeclarationSignature::Public(primary),
    };
    let superclasses = superclass_realizations(platform, bodies, &identity);
    let facts = with_realized_declarations(callees.clone(), &superclasses);
    let Some(callees) = facts
        .declared_instances_callees(class, bodies)
        .map_err(|reason| format!("an invalid object initialization: {reason:?}"))?
    else {
        return Ok(None);
    };
    let objects = facts
        .declared_instances_objects(class, bodies)
        .map_err(|reason| format!("an invalid object initialization: {reason:?}"))?;
    Ok(Some(DeclaredInstancesInitialization { callees, objects }))
}

/// The exact edges an object's admitted initialization executes, and the library objects it
/// reads as values.
struct DeclaredInstancesInitialization {
    callees: Vec<KlibDependencyCallable>,
    objects: Vec<crate::metadata::id_signature::KlibPublicIdSignature>,
}

/// The initialization edges of each object the closed bodies read, which the unit materializes
/// with every member dispatch reaches, as it materializes the object of a member it calls.
pub(super) fn read_class_initializations(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
) -> Result<Vec<KlibDependencyCallable>, String> {
    let mut edges = Vec::new();
    for class in callees.read_classes() {
        if let Some(initialization) =
            declared_instances_initialization(platform, bodies, callees, class)?
        {
            edges.extend(initialization.callees);
        }
    }
    Ok(edges)
}

/// The enum class whose implicit `values`, `valueOf` or `entries` getter `selected` is.
fn implicit_enum_member_class(
    platform: &dyn SemanticPlatform,
    selected: KlibBodyCallable<'_>,
) -> Option<crate::libraries::KlibBodyClassContract> {
    use crate::types::SemanticCallRole;
    if !matches!(
        selected.semantic_role(),
        Some(
            SemanticCallRole::KotlinEnumValues
                | SemanticCallRole::KotlinEnumValueOf
                | SemanticCallRole::KotlinEnumEntries
        )
    ) {
        return None;
    }
    let Some(crate::types::SemanticCallableOwner::Classifier(classifier)) =
        selected.declaration_owner()
    else {
        return None;
    };
    platform
        .klib_body_class_contract(classifier)
        .filter(|class| class.kind == crate::libraries::TypeKind::Enum)
}

/// The object declaring the member `identity`, when a support unit materializes it.
pub(super) fn member_object(
    platform: &dyn SemanticPlatform,
    identity: &KlibDependencyCallable,
) -> Option<crate::libraries::KlibBodyClassContract> {
    let (_, realization) = platform.external_callable_by_dependency(identity)?;
    let selected = realization.klib_body_callable()?;
    selected
        .class_contract()
        .filter(|class| {
            class.declares_instances()
                && class.emits_members()
                && selected.kind() == ExternalCallableKind::Member
        })
        .cloned()
}

/// The initialization edges of the object declaring the member `identity`, when a support unit
/// materializes the object, which every call of the member reads its instance from.
pub(super) fn member_object_initialization(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    identity: &KlibDependencyCallable,
) -> Result<Option<Vec<KlibDependencyCallable>>, String> {
    match member_object(platform, identity) {
        Some(class) => Ok(
            declared_instances_initialization(platform, bodies, callees, &class)?
                .map(|initialization| initialization.callees),
        ),
        None => Ok(None),
    }
}

/// The initialization edges of the library objects the body or initialization of `identity`
/// reads as values, which the unit materializes with every member dispatch reaches on them.
pub(super) fn read_object_initializations(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    identity: &KlibDependencyCallable,
    constructor: bool,
) -> Result<Vec<KlibDependencyCallable>, String> {
    let objects = if constructor {
        callees
            .constructor_initialization_objects(identity, bodies)
            .map_err(|reason| format!("an invalid class initialization: {reason:?}"))?
    } else {
        bodies.emitted_body_objects(identity, crate::klib_lowering::EMITTED_CALL_FORMS)
    };
    let mut edges = Vec::new();
    for class in objects
        .iter()
        .filter_map(crate::klib_lowering::object_classifier)
        .filter_map(|classifier| callees.read_class(classifier))
    {
        if let Some(initialization) =
            declared_instances_initialization(platform, bodies, callees, class)?
        {
            edges.extend(initialization.callees);
        }
    }
    Ok(edges)
}

/// The constructors that initialize the ordinary class `class`, when each one's admitted
/// initialization materializes the class: its primary constructor, or each constructor of a class
/// without one. Empty when the class is not materialized.
fn ordinary_class_constructors(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    class: &crate::libraries::KlibBodyClassContract,
) -> Result<Vec<KlibDependencyCallable>, String> {
    let Some(constructors) = crate::klib_lowering::initializing_constructors(class, bodies) else {
        return Ok(Vec::new());
    };
    for identity in &constructors {
        let Some((_, realization)) = platform.external_callable_by_dependency(identity) else {
            return Ok(Vec::new());
        };
        let Some(constructor) = realization.klib_body_callable() else {
            return Ok(Vec::new());
        };
        // A class whose constructor has no serialized declaration, such as one of a module
        // compiled from source alongside, is realized from its own declarations, never
        // materialized from a KLIB.
        if bodies.function_has_body(identity).is_none() {
            return Ok(Vec::new());
        }
        let superclasses = superclass_realizations(platform, bodies, identity);
        let admitted =
            with_realized_declarations(callees.with_declaration(constructor), &superclasses)
                .constructor_initialization_callees(identity, bodies)
                .map_err(|reason| format!("an invalid class initialization: {reason:?}"))?
                .is_some();
        if !admitted {
            return Ok(Vec::new());
        }
    }
    Ok(constructors)
}

/// The realizations of the superclass constructors a class's primary constructor `identity`
/// delegates through. Admitting the class's initialization admits theirs, which reads their
/// declarations, so a closure records them first.
fn superclass_realizations(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> Vec<crate::libraries::ExternalCallableRealization> {
    let mut realizations = Vec::new();
    let mut constructor = identity.clone();
    let mut visited = HashSet::default();
    while let Some(superclass) =
        crate::klib_lowering::delegated_superclass_constructor(bodies, &constructor)
    {
        if !visited.insert(superclass.clone()) {
            break;
        }
        let Some((_, realization)) = platform.external_callable_by_dependency(&superclass) else {
            break;
        };
        realizations.push(realization);
        constructor = superclass;
    }
    realizations
}

/// `facts` with the declarations of `realizations`.
fn with_realized_declarations<'a>(
    mut facts: crate::klib_lowering::KlibCalleeFacts<'a>,
    realizations: &'a [crate::libraries::ExternalCallableRealization],
) -> crate::klib_lowering::KlibCalleeFacts<'a> {
    for selected in realizations
        .iter()
        .filter_map(crate::libraries::ExternalCallableRealization::klib_body_callable)
    {
        facts.add_declaration(selected);
    }
    facts
}

/// The constructors which materialize the ordinary class declaring the member `identity`, one
/// of which created every instance the member is called on. Empty when no unit materializes it.
pub(super) fn member_class_constructors(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    identity: &KlibDependencyCallable,
) -> Result<Vec<KlibDependencyCallable>, String> {
    let Some((_, realization)) = platform.external_callable_by_dependency(identity) else {
        return Ok(Vec::new());
    };
    let Some(selected) = realization.klib_body_callable() else {
        return Ok(Vec::new());
    };
    match selected.class_contract() {
        Some(class)
            if !class.is_value
                && class.kind != crate::libraries::TypeKind::Enum
                && class.emits_members()
                && selected.kind() == ExternalCallableKind::Member =>
        {
            ordinary_class_constructors(platform, bodies, callees, class)
        }
        _ => Ok(Vec::new()),
    }
}

/// The checked edge facts the KLIB member hierarchy proved for `selected`: its inherited semantic
/// role and exact overridden declarations. A target recognizes a specially implemented member
/// through those facts.
fn member_edge(
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    selected: KlibBodyCallable<'_>,
) -> Result<crate::klib_lowering::CallEdge, String> {
    // The hierarchy decorates function calls. A property accessor is read as its property, which
    // carries neither a call role nor function override edges.
    if matches!(
        selected.identity().declaration,
        crate::libraries::KlibDeclarationSignature::Accessor(_)
    ) {
        return Ok(crate::klib_lowering::CallEdge::default());
    }
    callees
        .member_edge(selected.identity())
        .cloned()
        .ok_or_else(|| {
            format!(
                "the checker stored no member edge for KLIB declaration {:?}",
                selected.identity()
            )
        })
}

/// Whether `identity` is a member of a class no support unit materializes. Support bodies call it
/// through its declaration, or decline the call, so its body is never walked.
pub(super) fn is_external_member(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    identity: &KlibDependencyCallable,
) -> Result<bool, String> {
    let Some((_, realization)) = platform.external_callable_by_dependency(identity) else {
        return Ok(false);
    };
    let Some(selected) = realization.klib_body_callable() else {
        return Ok(false);
    };
    is_outside_units(platform, bodies, callees, selected)
}

/// Whether `callable` is a member of a class no support unit materializes, a builtin type of the
/// common type model included: a target which realizes declarations outside its emitted bodies
/// realizes it, and body lowering declines a call of it for any other target.
fn is_outside_units(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    callable: KlibBodyCallable<'_>,
) -> Result<bool, String> {
    Ok(callable.kind() == ExternalCallableKind::Member
        && !is_materialized_class_member(platform, bodies, callees, callable)?)
}

/// Walk the overrides a boxed instance of the value class `selected` belongs to dispatches to:
/// each one with an exact provider declaration, as are the declarations it overrides, is walked
/// with the class. The unit leaves any other slot unrealized.
fn walk_value_class_overrides(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    selected: crate::libraries::KlibBodyCallable<'_>,
    state: &mut ClosureState<'_, '_>,
) -> Result<(), String> {
    let overrides = crate::klib_lowering::value_class_overrides(selected, bodies)
        .map_err(|reason| format!("an invalid value class member: {reason:?}"))?;
    let provided =
        |identity: &KlibDependencyCallable| platform.external_callable_by_dependency(identity);
    for value_override in overrides {
        if provided(&value_override.identity).is_none()
            || !value_override
                .overridden
                .iter()
                .all(|overridden| provided(overridden).is_some())
        {
            continue;
        }
        for overridden in value_override.overridden {
            if state.declared.insert(overridden.clone()) {
                state
                    .declarations
                    .push(provided(&overridden).expect("checked above"));
            }
        }
        if state.dispatched.insert(value_override.identity.clone()) {
            state
                .pending
                .push_back((value_override.identity, selected.class_contract().cloned()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrFile, IrFunction};
    use crate::types::Ty;

    #[test]
    fn intrinsic_scalar_roots_ignore_orphan_external_call_nodes() {
        let mut ir = IrFile::default();
        let live_target = crate::fir::ExternalCallableId::from_raw(17);
        let orphan_target = crate::fir::ExternalCallableId::from_raw(23);
        let call = |ir: &mut IrFile, target| {
            ir.add_expr(IrExpr::Call {
                callee: Box::new(Callee::External {
                    target,
                    default_provider: None,
                    params: Vec::new(),
                    ret: Ty::Unit,
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                    extension_receiver_parameter: None,
                }),
                dispatch_receiver: None,
                args: Vec::new(),
            })
        };
        let body = call(&mut ir, live_target);
        call(&mut ir, orphan_target);
        ir.add_fun(IrFunction {
            name: "emitted".to_owned(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });

        let mut expected = HashSet::default();
        expected.insert(live_target);
        let actual = emitted_dependency_targets(&ir, |_, _| Ok(None))
            .unwrap()
            .into_iter()
            .map(|(_, target, _)| target)
            .collect::<HashSet<_>>();
        assert_eq!(actual, expected);
    }
}
