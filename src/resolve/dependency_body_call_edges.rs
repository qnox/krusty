//! Checked member-call facts for declarations selected by serialized KLIB bodies.

use crate::fir::{CheckedMemberCallEdge, ExternalCallableId};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

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
