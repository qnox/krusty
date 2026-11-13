//! Exact ordinary dependency declarations referenced by structurally emitted expression bodies.

use crate::backend::CheckedIrFile;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{Callee, IrCheckedOperation, IrExpr};
use crate::klib_libraries::{KlibDeclarationBodies, KlibIntrinsicFacts};
use crate::libraries::{
    ExternalCallableKind, ExternalCallableRealization, KlibBodyCallable, KlibDependencyCallable,
    SemanticPlatform,
};
use std::collections::VecDeque;

/// The declarations a unit's closure freezes, and the call edge facts of its members realized
/// outside the unit.
pub(super) struct FrozenClosure {
    pub(super) declarations: Vec<(crate::fir::ExternalCallableId, ExternalCallableRealization)>,
    pub(super) member_edges: HashMap<KlibDependencyCallable, crate::klib_lowering::CallEdge>,
    /// The library objects whose values the closure's bodies read, which the unit materializes.
    pub(super) read_objects: Vec<crate::libraries::KlibBodyClassContract>,
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
    let mut member_edges = HashMap::default();
    let mut state = ClosureState {
        pending: roots
            .callables
            .iter()
            .map(|root| (root.identity().clone(), None))
            .collect(),
        declarations: Vec::new(),
        declared: HashSet::default(),
        objects: HashSet::default(),
        read_objects: root_objects.to_vec(),
        dispatched: HashSet::default(),
        capabilities,
    };
    // An object the emitted source reads as a value is materialized as one a body reads.
    for class in root_objects {
        walk_object(
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
        if let Some(class) = member_class.filter(|class| {
            platform
                .external_callable_by_dependency(&identity)
                .is_none()
                && !externals.contains_key(&identity)
                && crate::klib_lowering::is_private_class_member(bodies, &identity, class)
        }) {
            state.pending.extend(
                bodies
                    .emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS)
                    .into_iter()
                    .map(|callee| (callee, Some(class.clone()))),
            );
            let objects =
                bodies.emitted_body_objects(&identity, crate::klib_lowering::EMITTED_CALL_FORMS);
            read_values(platform, bodies, capabilities.callees, &objects, &mut state)?;
            continue;
        }
        // A top-level function private to its file has no provider declaration either; only a
        // body of its own module calls it, and it is emitted with that body.
        if let crate::libraries::KlibDeclarationSignature::FileLocal { .. } = identity.declaration {
            state.pending.extend(
                bodies
                    .emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS)
                    .into_iter()
                    .map(|callee| (callee, None)),
            );
            let objects =
                bodies.emitted_body_objects(&identity, crate::klib_lowering::EMITTED_CALL_FORMS);
            read_values(platform, bodies, capabilities.callees, &objects, &mut state)?;
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
        let (declaration_target, realization) = match platform
            .external_callable_by_dependency(&identity)
        {
            Some(declaration) => declaration,
            None if raw_header_capability && known_target.is_none() => {
                state.pending.extend(
                    bodies
                        .emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS)
                        .into_iter()
                        .map(|callee| (callee, None)),
                );
                let objects = bodies
                    .emitted_body_objects(&identity, crate::klib_lowering::EMITTED_CALL_FORMS);
                read_values(platform, bodies, capabilities.callees, &objects, &mut state)?;
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
        let superclasses = superclass_realizations(platform, bodies, &identity);
        let facts = with_realized_declarations(
            capabilities.callees.with_declaration(selected),
            &superclasses,
        );
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
            state.dispatched.extend(
                facts
                    .constructor_dispatched_members(&identity, bodies)
                    .map_err(|reason| format!("an invalid class member: {reason:?}"))?,
            );
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
        let outside_edge = if member || is_outside_units(platform, bodies, &facts, selected)? {
            Some(member_edge(platform, declaration_target, selected)?)
        } else {
            None
        };
        let external_member = outside_edge
            .as_ref()
            .is_some_and(|edge| capabilities.target_realizes_bodyless || realizes_any_member(edge));
        // A call of a member realized outside the unit carries the facts the member hierarchy
        // decorates a source call of it with; the edge is what authorizes the call.
        if let Some(edge) = outside_edge.filter(|_| external_member) {
            member_edges.insert(identity.clone(), edge);
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
                .is_some_and(|modifiers| modifiers.permits_ordinary_body()))
        {
            return Err(format!("a transitive KLIB body has no supported ordinary declaration contract: {identity:?}"));
        }
        if emits_body {
            // A member of an ordinary class is called on an instance its primary constructor
            // created, which materializes the class.
            if let Some(primary) = selected
                .class_contract()
                .filter(|class| {
                    !class.is_value
                        && class.kind != crate::libraries::TypeKind::Object
                        && selected.kind() == ExternalCallableKind::Member
                })
                .map(|class| ordinary_class_constructor(platform, bodies, &facts, class))
                .transpose()?
                .flatten()
            {
                state.pending.push_back((primary, None));
            }
            // A member of an object reads the instance the object's own initialization created,
            // which materializes the object once with every member dispatch reaches. So does a
            // body reading an object's value, unless the target represents that object itself.
            if let Some(class) = member_object(platform, &identity) {
                walk_object(platform, bodies, &facts, class, &mut state)?;
            }
            let objects =
                bodies.emitted_body_objects(&identity, crate::klib_lowering::EMITTED_CALL_FORMS);
            read_values(platform, bodies, &facts, &objects, &mut state)?;
            let member_class = selected
                .class_contract()
                .filter(|class| class.emits_members())
                .cloned();
            state.pending.extend(
                bodies
                    .emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS)
                    .into_iter()
                    .map(|callee| (callee, member_class.clone())),
            );
        }
        state.declarations.push((declaration_target, realization));
    }
    Ok(FrozenClosure {
        declarations: state.declarations,
        member_edges,
        read_objects: state.read_objects,
    })
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
    read_objects: Vec<crate::libraries::KlibBodyClassContract>,
    /// Members of a materialized class that dispatch reaches: their bodies are emitted even where
    /// a call of one folds to a common operation, as a virtual call runs the body.
    dispatched: HashSet<KlibDependencyCallable>,
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
            .filter(|class| {
                class.kind == crate::libraries::TypeKind::Object && class.emits_members()
            })
        else {
            continue;
        };
        // An object whose initialization no unit admits is realized where its own module exports
        // its instance, as a call of its member is.
        if object_initialization(platform, bodies, facts, &class)?.is_none() {
            continue;
        }
        if !state
            .read_objects
            .iter()
            .any(|object| object.classifier == class.classifier)
        {
            state.read_objects.push(class.clone());
        }
        walk_object(platform, bodies, facts, class, state)?;
    }
    Ok(())
}

/// Walk the initialization of the object `class` once: the object is materialized with every
/// member dispatch reaches, and the declarations its members override are named.
fn walk_object(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    facts: &crate::klib_lowering::KlibCalleeFacts<'_>,
    class: crate::libraries::KlibBodyClassContract,
    walk: &mut ClosureState<'_, '_>,
) -> Result<(), String> {
    if !walk.objects.insert(class.classifier) {
        return Ok(());
    }
    let Some(initialization) = object_initialization(platform, bodies, facts, &class)? else {
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
            .object_dispatched_members(&class, bodies)
            .map_err(|reason| format!("an invalid object member: {reason:?}"))?,
    );
    for overridden in facts
        .object_overridden_declarations(&class, bodies)
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
            .filter(|class| {
                class.kind == crate::libraries::TypeKind::Object && class.emits_members()
            })
        else {
            continue;
        };
        if object_initialization(platform, bodies, callees, &class)?.is_some() {
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
    target_realizes_intrinsic: &dyn Fn(crate::libraries::CompilerIntrinsic) -> bool,
    target_represents_classifier: &dyn Fn(crate::types::TypeName) -> bool,
) -> Result<Vec<KlibBodyCallable<'a>>, String> {
    let mut roots = Vec::new();
    let mut seen = HashSet::default();
    for expression in crate::ir::emitted_body_roots(&source.ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(&source.ir, root))
    {
        let target = match source.ir.expr(expression) {
            IrExpr::Call { callee, .. } => match &**callee {
                // A call leaving arguments out reaches the same body through its default entry.
                Callee::External { target, .. } => *target,
                _ => continue,
            },
            IrExpr::New {
                external_target: Some(target),
                ..
            } => *target,
            IrExpr::Checked(operation) => match &**operation {
                IrCheckedOperation::ExternalPropertyRead { target, .. } => {
                    source
                        .callables
                        .property(*target)
                        .ok_or("an emitted dependency property has no frozen fact")?
                        .getter
                }
                IrCheckedOperation::ExternalPropertyWrite { target, .. } => source
                    .callables
                    .property(*target)
                    .ok_or("an emitted dependency property has no frozen fact")?
                    .setter
                    .ok_or("an emitted dependency property write has no selected setter")?,
                _ => continue,
            },
            _ => continue,
        };
        let callable = source
            .callables
            .callable(target)
            .ok_or("an emitted dependency callable has no frozen fact")?;
        let Some(selected) = source.callables.klib_body_callable_at(expression, target) else {
            continue;
        };
        let construction = matches!(source.ir.expr(expression), IrExpr::New { .. });
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
            if callees
                .constructor_initialization_callees(selected.identity(), bodies)
                .map_err(|reason| format!("an invalid constructor root: {reason:?}"))?
                .is_some()
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
        // A source call of a member of an open or abstract class dispatches on an instance of any
        // subclass, including one the target lays out itself (an array's `IntIterator`). Only a
        // final class's instances are all built by the constructor the unit materializes.
        if !callable.is_top_level()
            && selected.class_contract().is_some_and(|class| {
                !class.is_value
                    && class.modality != crate::metadata::semantic::KotlinModality::Final
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
        let (target, _) = platform
            .external_callable_by_dependency(callable.identity())
            .ok_or("a builtin-type member has no provider declaration")?;
        return Ok(!realizes_any_member(&member_edge(
            platform, target, callable,
        )?));
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
    if class.kind == crate::libraries::TypeKind::Object {
        return Ok(object_initialization(platform, bodies, callees, class)?.is_some());
    }
    Ok(ordinary_class_constructor(platform, bodies, callees, class)?.is_some())
}

/// The initialization edges of the object `class`, when its admitted initialization materializes
/// it. Its private primary constructor has no provider declaration: the object's own members
/// carry its class, and only its declaration creates its instance.
fn object_initialization(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    class: &crate::libraries::KlibBodyClassContract,
) -> Result<Option<ObjectInitialization>, String> {
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
        .object_initialization_callees(class, bodies)
        .map_err(|reason| format!("an invalid object initialization: {reason:?}"))?
    else {
        return Ok(None);
    };
    let objects = facts
        .object_initialization_objects(class, bodies)
        .map_err(|reason| format!("an invalid object initialization: {reason:?}"))?;
    Ok(Some(ObjectInitialization { callees, objects }))
}

/// The exact edges an object's admitted initialization executes, and the library objects it
/// reads as values.
struct ObjectInitialization {
    callees: Vec<KlibDependencyCallable>,
    objects: Vec<crate::metadata::id_signature::KlibPublicIdSignature>,
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
            class.kind == crate::libraries::TypeKind::Object
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
        Some(class) => Ok(object_initialization(platform, bodies, callees, &class)?
            .map(|initialization| initialization.callees)),
        None => Ok(None),
    }
}

/// The primary constructor of the ordinary class `class`, when its admitted initialization
/// materializes the class.
fn ordinary_class_constructor(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    class: &crate::libraries::KlibBodyClassContract,
) -> Result<Option<KlibDependencyCallable>, String> {
    let Some(primary) = class.primary_constructor.clone() else {
        return Ok(None);
    };
    let identity = KlibDependencyCallable {
        module: class.module.clone(),
        declaration: crate::libraries::KlibDeclarationSignature::Public(primary),
    };
    let Some((_, realization)) = platform.external_callable_by_dependency(&identity) else {
        return Ok(None);
    };
    let Some(constructor) = realization.klib_body_callable() else {
        return Ok(None);
    };
    // A class whose constructor has no serialized declaration, such as one of a module compiled
    // from source alongside, is realized from its own declarations, never materialized from a KLIB.
    if bodies.function_has_body(&identity).is_none() {
        return Ok(None);
    }
    let superclasses = superclass_realizations(platform, bodies, &identity);
    let admitted = with_realized_declarations(callees.with_declaration(constructor), &superclasses)
        .constructor_initialization_callees(&identity, bodies)
        .map_err(|reason| format!("an invalid class initialization: {reason:?}"))?
        .is_some();
    Ok(admitted.then_some(identity))
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
        facts = facts.with_declaration(selected);
    }
    facts
}

/// The primary constructor which materializes the ordinary class declaring the member
/// `identity`, which every instance the member is called on was created by.
pub(super) fn member_class_constructor(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    callees: &crate::klib_lowering::KlibCalleeFacts<'_>,
    identity: &KlibDependencyCallable,
) -> Result<Option<KlibDependencyCallable>, String> {
    let Some((_, realization)) = platform.external_callable_by_dependency(identity) else {
        return Ok(None);
    };
    let Some(selected) = realization.klib_body_callable() else {
        return Ok(None);
    };
    match selected.class_contract() {
        Some(class)
            if !class.is_value
                && class.emits_members()
                && selected.kind() == ExternalCallableKind::Member =>
        {
            ordinary_class_constructor(platform, bodies, callees, class)
        }
        _ => Ok(None),
    }
}

/// The facts the member hierarchy of `selected`'s declaring class decorates a source call of
/// `selected` (frozen as `target`) with: the declarations it overrides and the semantic role it
/// inherits from them. A target recognizes a member it implements specially through them.
fn member_edge(
    platform: &dyn SemanticPlatform,
    target: crate::fir::ExternalCallableId,
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
    let Some(crate::types::SemanticCallableOwner::Classifier(owner)) = selected.declaration_owner()
    else {
        return Err(format!(
            "a KLIB member has no declaring class: {:?}",
            selected.identity()
        ));
    };
    let callables = crate::symbol_resolver::member_declarations_in_hierarchy(
        platform,
        crate::types::Ty::obj_name(owner),
        selected.name(),
    );
    let mut found = callables
        .functions()
        .iter()
        .filter(|function| function.callable.external_identity == Some(target));
    let (Some(function), None) = (found.next(), found.next()) else {
        return Err(format!(
            "a KLIB member is not one declaration of its class's hierarchy: {:?}",
            selected.identity()
        ));
    };
    Ok(crate::klib_lowering::CallEdge {
        semantic_role: function.callable.semantic_role,
        overridden: function.callable.overridden_declarations.clone(),
    })
}

/// Whether a call with `edge` invokes one of `kotlin.Any`'s members, which every target realizes
/// from the receiver's own representation.
fn realizes_any_member(edge: &crate::klib_lowering::CallEdge) -> bool {
    matches!(
        edge.semantic_role,
        Some(
            crate::types::SemanticCallRole::KotlinAnyEquals
                | crate::types::SemanticCallRole::KotlinAnyHashCode
                | crate::types::SemanticCallRole::KotlinAnyToString
        )
    )
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
