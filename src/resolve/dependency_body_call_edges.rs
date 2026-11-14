//! Checked member-call facts for declarations selected by serialized KLIB bodies.

#[path = "klib_type_facts.rs"]
mod klib_type_facts;

use crate::fir::ExternalCallableId;
use crate::ir::{CheckedKlibDependencyFacts, CheckedMemberCallEdge};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// Check and freeze semantic facts for the body graph reachable from the source file's selected
/// KLIB declarations. Backends may later choose a target-specific subset, but they never resolve
/// member hierarchy edges or builtin implementations while doing so.
pub(crate) fn for_checked_klib_bodies<'a>(
    source: &dyn SymbolSource,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
    roots: impl IntoIterator<Item = crate::libraries::KlibBodyCallable<'a>>,
) -> Result<CheckedKlibDependencyFacts, ExternalCallableId> {
    let mut builtin_implementations = crate::fx_hash::HashMap::default();
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
        classifier_roots.extend(bodies.type_classifiers_in_body(&identity));
        pending.extend(bodies.direct_callees(&identity));
        pending.extend(
            bodies.emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS),
        );

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
        let Some(selected) = realization.klib_body_callable() else {
            continue;
        };
        semantic_types.extend(selected.params().iter().copied());
        semantic_types.push(selected.ret());
        semantic_types.extend(selected.type_parameters().iter().copied());
        #[cfg(test)]
        if realization.kind == crate::libraries::ExternalCallableKind::Constructor {
            reached_constructors.insert(identity.clone());
        }
        if realization.kind != crate::libraries::ExternalCallableKind::Member {
            continue;
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
        let implementation_name = selected
            .accessor()
            .map_or_else(|| selected.name(), |accessor| accessor.name.as_ref());
        let implementations = crate::symbol_resolver::builtin_implementation_facts(
            source,
            owner,
            implementation_name,
            external,
        );
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
    );
    #[cfg(test)]
    let mut facts = facts;
    #[cfg(test)]
    for constructor in reached_constructors {
        facts.record_reached_constructor(constructor);
    }
    Ok(facts)
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
