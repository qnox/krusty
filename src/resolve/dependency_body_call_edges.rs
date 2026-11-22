//! Checked member-call facts for declarations selected by serialized KLIB bodies.

#[path = "klib_sam_facts.rs"]
mod klib_sam_facts;
#[path = "klib_type_facts.rs"]
mod klib_type_facts;

use crate::fir::ExternalCallableId;
use crate::ir::{CheckedKlibDependencyFacts, CheckedMemberCallEdge};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// Check and freeze semantic facts for the body graph reachable from the source file's selected
/// KLIB declarations. Backends may later choose a target-specific subset, but they never resolve
/// member hierarchy edges or builtin implementations while doing so. Each member's builtin
/// implementations include the overrides of the target's `represented` classes below its owner,
/// as the source handoff records them.
pub(crate) fn for_checked_klib_bodies<'a>(
    source: &dyn SymbolSource,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
    roots: impl IntoIterator<Item = crate::libraries::KlibBodyCallable<'a>>,
    represented: &[TypeName],
) -> Result<CheckedKlibDependencyFacts, ExternalCallableId> {
    let mut builtin_implementations = crate::fx_hash::HashMap::default();
    let mut sam_declarations = crate::fx_hash::HashMap::default();
    let mut member_edges = crate::fx_hash::HashMap::default();
    #[cfg(test)]
    let mut reached_constructors = crate::fx_hash::HashSet::default();
    let roots = roots.into_iter().collect::<Vec<_>>();
    let mut classifier_roots = Vec::new();
    let mut semantic_types = Vec::new();
    for callable in &roots {
        semantic_types.extend(callable.params().iter().copied());
        semantic_types.push(callable.ret());
        semantic_types.extend(callable.type_parameters().iter().copied());
    }
    let mut pending = roots
        .into_iter()
        .map(|callable| callable.identity().clone())
        .collect::<Vec<_>>();
    let mut visited = crate::fx_hash::HashSet::default();
    while let Some(identity) = pending.pop() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        let emitted = crate::klib_lowering::emitted_body(bodies, &identity);
        classifier_roots.extend(bodies.type_classifiers_in_body(&identity));
        classifier_roots.extend(emitted.hierarchy_roots);
        for (signature, name) in emitted.sam_classifiers {
            let key = (identity.module.clone(), signature.clone());
            if sam_declarations.contains_key(&key) {
                continue;
            }
            if let Some(declaration) =
                klib_sam_facts::freeze(source, bodies, &identity.module, &signature, name)
            {
                sam_declarations.insert(key, declaration);
            }
        }
        pending.extend(bodies.direct_callees(&identity));
        pending.extend(bodies.overridden_callables(&identity));
        pending.extend(emitted.callables);

        // An object the body reads as a value is materialized with its initialization and the
        // members dispatch reaches, as the emission closure walks it.
        pending.extend(read_object_callees(source, bodies, &emitted.objects));

        // A reached constructor's initialization is serialized on its owning class. These are
        // declaration facts only: common class initialization still selects executable roots.
        pending.extend(bodies.constructor_initializer_callables(
            &identity,
            crate::klib_lowering::EMITTED_CALL_FORMS,
        ));

        let Some((external, realization)) = source.external_callable_by_dependency(&identity)
        else {
            continue;
        };
        if realization.dependency_callable.as_ref() != Some(&identity) {
            return Err(external);
        }
        // Every reached member is dispatched by the lowered body, including one with no body of
        // its own (`Iterator.next`, `Any.equals`). Its builtin implementations are a fact of the
        // declaration, under the same property-or-function name the source handoff uses.
        if realization.kind == crate::libraries::ExternalCallableKind::Member {
            let implementations = super::RepresentedOverrides::new(source, represented).complete(
                external,
                &super::builtin_implementation::for_external_member(source, external)?,
            )?;
            match builtin_implementations.entry(external) {
                std::collections::hash_map::Entry::Occupied(existing)
                    if existing.get() != &implementations =>
                {
                    return Err(external);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(implementations);
                }
            }
        }
        let Some(selected) = realization.klib_body_callable() else {
            continue;
        };
        // A continuation interception's call expands to its coroutine class's own interception.
        match crate::klib_lowering::expansion_member(selected, bodies) {
            Ok(member) => pending.extend(member),
            Err(_) => return Err(external),
        }
        semantic_types.extend(selected.params().iter().copied());
        semantic_types.push(selected.ret());
        semantic_types.extend(selected.type_parameters().iter().copied());
        // Emission lowers a member of an ordinary class with the class the constructors that
        // initialize it materialize, and closes their initialization with the member: the
        // members dispatch reaches on the class included.
        if let Some(class) = selected.class_contract().filter(|class| {
            !class.is_value && selected.kind() == crate::libraries::ExternalCallableKind::Member
        }) {
            pending.extend(
                crate::klib_lowering::initializing_constructors(class, bodies).unwrap_or_default(),
            );
        }
        // Emission closes a constructor through the same initialization plan: superclass
        // arguments, field and block initializers, dispatched members, and a materialized
        // superclass constructor. Initializer-root symbols omit the members dispatch reaches,
        // so an override emitted with the class can call `kotlin.Any.toString` without the
        // checker ever freezing that edge.
        if realization.kind == crate::libraries::ExternalCallableKind::Constructor {
            pending.extend(emitted_initialization_callees(
                source, bodies, &identity, selected,
            ));
        }
        // A boxed value class's instances dispatch to its overrides, which emission walks with
        // a construction or a member of the class.
        if matches!(
            realization.kind,
            crate::libraries::ExternalCallableKind::Constructor
                | crate::libraries::ExternalCallableKind::Member
        ) && selected
            .class_contract()
            .is_some_and(crate::libraries::KlibBodyClassContract::has_unit_box)
        {
            // A class whose overrides do not authenticate is declined by emission with its
            // own reason; there are no override edges to freeze for it.
            if let Ok(overrides) = crate::klib_lowering::value_class_overrides(selected, bodies) {
                pending.extend(overrides.into_iter().map(|value| value.identity));
            }
        }
        #[cfg(test)]
        if realization.kind == crate::libraries::ExternalCallableKind::Constructor {
            reached_constructors.insert(identity.clone());
        }
        if realization.kind != crate::libraries::ExternalCallableKind::Member {
            continue;
        }
        // Every instance a member of a materialized ordinary class is called on was created by
        // that class's primary constructor, so emission materializes the class with the member
        // and closes its initialization and dispatched members. The walk reaches them the same way.
        if let Some(class) = selected.class_contract().filter(|class| {
            !class.is_value
                && class.kind != crate::libraries::TypeKind::Enum
                && class.emits_members()
        }) {
            if let Some(primary) = class.primary_constructor.clone() {
                pending.push(crate::libraries::KlibDependencyCallable {
                    module: class.module.clone(),
                    declaration: crate::libraries::KlibDeclarationSignature::Public(primary),
                });
            }
        }
        let Some(crate::types::SemanticCallableOwner::Classifier(owner)) =
            selected.declaration_owner()
        else {
            return Err(external);
        };
        if realization.callable.external_identity != Some(external)
            || realization.declaration_owner
                != Some(crate::types::SemanticCallableOwner::Classifier(owner))
        {
            return Err(external);
        }
        if !matches!(
            &identity.declaration,
            crate::libraries::KlibDeclarationSignature::Accessor(_)
        ) {
            let edge = klib_dependency_member_call_edge(source, owner, selected.name(), external)?;
            member_edges.insert(identity, edge);
        }
    }
    let type_hierarchy =
        klib_type_facts::freeze(source, classifier_roots, std::iter::empty(), semantic_types);
    let facts = CheckedKlibDependencyFacts::new(
        builtin_implementations,
        member_edges,
        Some(type_hierarchy),
    )
    .with_sam_declarations(sam_declarations);
    #[cfg(test)]
    let mut facts = facts;
    #[cfg(test)]
    for constructor in reached_constructors {
        facts.record_reached_constructor(constructor);
    }
    Ok(facts)
}

/// Declarations the emission closure reaches from `identity`'s class initialization.
///
/// A plan that does not admit the class contributes nothing. Emission declines that constructor
/// on its own; this walk must not turn that decline into a missing member-edge failure.
fn emitted_initialization_callees(
    source: &dyn SymbolSource,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
    identity: &crate::libraries::KlibDependencyCallable,
    selected: crate::libraries::KlibBodyCallable<'_>,
) -> Vec<crate::libraries::KlibDependencyCallable> {
    let chain = superclass_realizations(source, bodies, identity);
    with_chain(
        crate::klib_lowering::KlibCalleeFacts::new([selected]),
        &chain,
    )
    .constructor_initialization_callees(identity, bodies)
    .ok()
    .flatten()
    .unwrap_or_default()
}

/// Declarations the emission closure reaches through the library objects `objects` names: each
/// admitted object's initialization, with the members dispatch reaches, and those of the objects
/// that initialization reads in turn. An object no unit admits contributes nothing.
fn read_object_callees(
    source: &dyn SymbolSource,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
    objects: &[crate::metadata::id_signature::KlibPublicIdSignature],
) -> Vec<crate::libraries::KlibDependencyCallable> {
    let mut callees = Vec::new();
    let mut pending = objects.to_vec();
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(object) = pending.pop() {
        let Some(class) = crate::klib_lowering::object_classifier(&object)
            .filter(|classifier| seen.insert(*classifier))
            .and_then(|classifier| source.klib_body_class_contract(classifier))
            .filter(|class| {
                class.kind == crate::libraries::TypeKind::Object && class.emits_members()
            })
        else {
            continue;
        };
        let Some(primary) = class.primary_constructor.clone() else {
            continue;
        };
        let constructor = crate::libraries::KlibDependencyCallable {
            module: class.module.clone(),
            declaration: crate::libraries::KlibDeclarationSignature::Public(primary),
        };
        let chain = superclass_realizations(source, bodies, &constructor);
        let facts = with_chain(crate::klib_lowering::KlibCalleeFacts::new([]), &chain);
        if let Ok(Some(initialization)) = facts.object_initialization_callees(&class, bodies) {
            callees.extend(initialization);
        }
        pending.extend(
            facts
                .object_initialization_objects(&class, bodies)
                .unwrap_or_default(),
        );
    }
    callees
}

/// The realizations of the superclass constructors `constructor` delegates to, which the
/// emission closure admits an initialization with.
fn superclass_realizations(
    source: &dyn SymbolSource,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
    constructor: &crate::libraries::KlibDependencyCallable,
) -> Vec<crate::libraries::ExternalCallableRealization> {
    let mut chain = Vec::new();
    let mut constructor = constructor.clone();
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(superclass) =
        crate::klib_lowering::delegated_superclass_constructor(bodies, &constructor)
    {
        if !seen.insert(superclass.clone()) {
            break;
        }
        let Some((_, realization)) = source.external_callable_by_dependency(&superclass) else {
            break;
        };
        chain.push(realization);
        constructor = superclass;
    }
    chain
}

/// `facts` with the declarations `chain` realizes.
fn with_chain<'a>(
    mut facts: crate::klib_lowering::KlibCalleeFacts<'a>,
    chain: &'a [crate::libraries::ExternalCallableRealization],
) -> crate::klib_lowering::KlibCalleeFacts<'a> {
    for realization in chain {
        if let Some(callable) = realization.klib_body_callable() {
            facts = facts.with_declaration(callable);
        }
    }
    facts
}

/// Resolve the exact declaration in its common member hierarchy and freeze the edge facts that
/// KLIB dependency-body lowering needs. The body symbol already selected `identity`; this only
/// checks that provider identity against the common hierarchy and records its checked semantics.
pub(crate) fn klib_dependency_member_call_edge(
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &str,
    identity: ExternalCallableId,
) -> Result<CheckedMemberCallEdge, ExternalCallableId> {
    let callables =
        crate::symbol_resolver::member_declarations_in_hierarchy(source, Ty::obj_name(owner), name);
    let mut found = callables
        .functions()
        .iter()
        .filter(|function| function.callable.external_identity == Some(identity));
    let (Some(function), None) = (found.next(), found.next()) else {
        return Err(identity);
    };
    Ok(CheckedMemberCallEdge {
        semantic_role: function.callable.semantic_role,
        overridden: function.callable.overridden_declarations.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::EmptySymbolSource;
    use crate::types::{wk, SemanticCallRole};

    #[test]
    fn klib_edge_facts_preserve_the_exact_builtin_declaration() {
        let source = EmptySymbolSource;
        let (functions, _) = crate::symbol_resolver::declared_member_declarations(
            &source,
            Ty::obj_name(wk::any()),
            "toString",
        )
        .into_parts();
        let [declaration] = functions.overloads.as_slice() else {
            panic!("the core model declares one Any.toString member");
        };
        let identity = declaration
            .callable
            .external_identity
            .expect("the core declaration has a stable identity");

        let edge = klib_dependency_member_call_edge(&source, wk::any(), "toString", identity)
            .expect("the exact hierarchy declaration is checked");

        assert_eq!(
            edge.semantic_role,
            Some(SemanticCallRole::KotlinAnyToString)
        );
        assert!(edge.overridden.is_empty());
        assert_eq!(
            klib_dependency_member_call_edge(
                &source,
                wk::any(),
                "toString",
                ExternalCallableId::from_raw(u32::MAX),
            ),
            Err(ExternalCallableId::from_raw(u32::MAX))
        );
    }
}
