//! Resolver-owned collection of the hierarchy frozen for selected KLIB bodies.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::CheckedKlibTypeFacts;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind, TypeName};

pub(super) fn freeze(
    source: &dyn SymbolSource,
    classifier_roots: impl IntoIterator<Item = TypeName>,
    type_variable_roots: impl IntoIterator<Item = String>,
    semantic_types: impl IntoIterator<Item = Ty>,
) -> CheckedKlibTypeFacts {
    let mut classifiers = HashMap::default();
    let mut actualized_expects = HashMap::default();
    let mut flexible_upper_bounds = HashMap::default();
    let mut type_variable_bounds = HashMap::default();
    let mut pending = classifier_roots.into_iter().collect::<Vec<_>>();
    let mut visited = HashSet::default();
    let mut variables = type_variable_roots.into_iter().collect::<HashSet<_>>();
    for ty in semantic_types {
        collect_type(ty, &mut pending, &mut variables);
    }

    while let Some(identity) = pending.pop() {
        if !visited.insert(identity) {
            continue;
        }
        if let Some(actual) = source.actualized_expect(identity) {
            actualized_expects.insert(identity, actual);
            pending.push(actual);
        }
        let Some(classifier) = source.classifier(identity) else {
            continue;
        };
        classifiers.insert(identity, classifier.clone());
        if let Some(actual) = source.actualized_expect(identity) {
            classifiers.insert(actual, classifier.clone());
        }
        for parent in classifier.supertypes.iter_ids() {
            pending.push(parent);
        }
        for &supertype in &classifier.supertype_templates {
            collect_type(supertype, &mut pending, &mut variables);
        }
        for bounds in classifier.type_param_bounds() {
            for &bound in bounds {
                collect_type(bound, &mut pending, &mut variables);
            }
        }
        if let TyKind::Obj(upper, _) = source
            .platform_flexible_upper_bound(Ty::obj_name(identity))
            .kind()
        {
            if upper != identity {
                flexible_upper_bounds.insert(identity, upper);
                pending.push(upper);
            }
        }
    }

    for variable in variables {
        type_variable_bounds.insert(
            variable.clone(),
            source.type_variable_bounds(&variable).into_boxed_slice(),
        );
    }
    CheckedKlibTypeFacts::from_records(
        classifiers,
        actualized_expects,
        flexible_upper_bounds,
        type_variable_bounds,
    )
}

fn collect_type(ty: Ty, classifiers: &mut Vec<TypeName>, variables: &mut HashSet<String>) {
    match ty.kind() {
        TyKind::Obj(owner, arguments) => {
            classifiers.push(owner);
            for &argument in arguments.iter() {
                collect_type(argument, classifiers, variables);
            }
        }
        TyKind::Fun(signature) => {
            for &parameter in &signature.params {
                collect_type(parameter, classifiers, variables);
            }
            collect_type(signature.ret, classifiers, variables);
        }
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner)
        | TyKind::InProjection(inner)
        | TyKind::OutProjection(inner)
        | TyKind::StarProjection(inner)
        | TyKind::RawProjection(inner)
        | TyKind::DefinitelyNotNull(inner) => collect_type(inner, classifiers, variables),
        TyKind::TyParam(name, bound) | TyKind::Var(name, bound) => {
            variables.insert(name.as_str().to_owned());
            collect_type(bound, classifiers, variables);
        }
        TyKind::Intersection(parts) => {
            for &part in parts.iter() {
                collect_type(part, classifiers, variables);
            }
        }
        TyKind::Unit | TyKind::Null | TyKind::Nothing | TyKind::Error | TyKind::Pending => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assignable::{is_subtype, TyCtx, TypeOracle};
    use crate::libraries::{ClassifierDeclaration, LibraryType, ResolvedSymbols, TypeParameters};
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::type_name;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;

    struct ClassDeclarations(Cell<bool>);

    impl SymbolSource for ClassDeclarations {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
            let classifier = if !self.0.get() {
                None
            } else {
                namespace.existing_classifier(name).and_then(|identity| {
                    let any = Ty::obj("kotlin/Any");
                    let mut declaration = LibraryType::declaration_header();
                    declaration.type_parameters =
                        TypeParameters::invariant(vec!["T".to_owned()], vec![vec![any]]);
                    match identity {
                        child if child == type_name("fixture/Child") => {
                            declaration.supertypes = vec![type_name("fixture/Parent")].into();
                            declaration.supertype_templates =
                                vec![Ty::obj_args("fixture/Parent", &[Ty::ty_param("T", any)])];
                            Some((identity, Arc::new(declaration)))
                        }
                        parent if parent == type_name("fixture/Parent") => {
                            declaration.supertypes = vec![crate::types::wk::any()].into();
                            Some((identity, Arc::new(declaration)))
                        }
                        _ => None,
                    }
                })
            };
            let (classifier_name, classifier, classifier_declaration) = match classifier {
                Some((identity, classifier)) => (
                    Some(identity),
                    Some(classifier),
                    Some(ClassifierDeclaration::Ordinary(identity)),
                ),
                None => (None, None, None),
            };
            Rc::new(ResolvedSymbols {
                classifier_name,
                classifier,
                classifier_declaration,
                ..ResolvedSymbols::default()
            })
        }
    }

    #[test]
    fn hierarchy_facts_remain_sufficient_after_provider_access_ends() {
        let source = ClassDeclarations(Cell::new(true));
        let facts = freeze(
            &source,
            [type_name("fixture/Child")],
            std::iter::empty(),
            std::iter::empty(),
        );
        source.0.set(false);

        let child = Ty::obj_args("fixture/Child", &[Ty::String]);
        let parent = Ty::obj_args("fixture/Parent", &[Ty::String]);
        let mismatched = Ty::obj_args("fixture/Parent", &[Ty::Int]);
        let raw_child = Ty::obj_args("fixture/Child", &[Ty::raw_projection(Ty::String)]);
        let raw_parent = Ty::obj_args("fixture/Parent", &[Ty::raw_projection(Ty::String)]);
        assert!(is_subtype(&TyCtx::new(), &facts, child, parent));
        assert!(!is_subtype(&TyCtx::new(), &facts, child, mismatched));
        assert_eq!(facts.direct_supertypes(raw_child), vec![raw_parent]);
    }
}
