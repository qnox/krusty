//! Checked member-call facts for declarations selected by serialized KLIB bodies.

use crate::fir::{CheckedMemberCallEdge, ExternalCallableId};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// Member facts for every exact KLIB body edge reachable from the declarations a checked source
/// file selected. The checker builds this table before the backend receives the file.
#[derive(Default)]
pub(crate) struct CheckedKlibDependencyFacts {
    pub(crate) builtin_implementations:
        crate::fx_hash::HashMap<ExternalCallableId, Box<[crate::fir::BuiltinImplementation]>>,
    pub(crate) member_edges:
        crate::fx_hash::HashMap<crate::libraries::KlibDependencyCallable, CheckedMemberCallEdge>,
}

/// Check and freeze semantic facts for the body graph reachable from the source file's selected
/// KLIB declarations. Backends may later choose a target-specific subset, but they never resolve
/// member hierarchy edges or builtin implementations while doing so.
pub(crate) fn for_checked_klib_bodies<'a>(
    source: &dyn SymbolSource,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
    roots: impl IntoIterator<Item = crate::libraries::KlibBodyCallable<'a>>,
) -> Result<CheckedKlibDependencyFacts, ExternalCallableId> {
    let mut facts = CheckedKlibDependencyFacts::default();
    let mut pending = roots
        .into_iter()
        .map(|callable| callable.identity().clone())
        .collect::<Vec<_>>();
    let mut visited = crate::fx_hash::HashSet::default();
    while let Some(identity) = pending.pop() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        pending.extend(bodies.direct_callees(&identity));
        pending.extend(
            bodies.emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS),
        );

        // A reached constructor's initialization is serialized on its owning class. These are
        // declaration facts only: common class initialization still selects executable roots.
        pending.extend(
            bodies
                .constructor_member_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS),
        );

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
        match facts.builtin_implementations.entry(external) {
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
            facts.member_edges.insert(identity, edge);
        }
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
