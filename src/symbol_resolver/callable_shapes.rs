//! Callable shapes implemented by nominal classifiers.
//!
//! Invocation may derive a function signature through a classifier hierarchy. Declared-type
//! normalization is narrower: only Kotlin's function-type classifier representations become
//! `Ty::Fun`; callable objects keep their nominal member scope.

use super::*;

/// Every exact function signature implemented by an applied classifier. Providers attach symbolic
/// signatures to classifier records; core owns inheritance traversal and substitution. A classifier
/// may implement several unrelated function supertypes, so callers with an expected shape must
/// select from the complete set rather than accepting whichever hierarchy edge is visited first.
pub(crate) fn classifier_callable_signatures(source: &dyn SymbolSource, ty: Ty) -> Vec<Ty> {
    let mut queue = std::collections::VecDeque::from([ty]);
    let mut seen = std::collections::HashSet::new();
    let mut signatures = Vec::new();
    while let Some(current) = queue.pop_front() {
        if matches!(current.non_null(), Ty::Fun(_)) {
            let callable = current.non_null();
            if !signatures.contains(&callable) {
                signatures.push(callable);
            }
            continue;
        }
        let Some(internal) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(internal) {
            continue;
        }
        let Some(classifier) = source.classifier(internal) else {
            continue;
        };
        let bindings = classifier_bindings(&classifier, current);
        for signature in classifier
            .callable_signatures
            .iter()
            .copied()
            .chain(classifier.callable_signature)
        {
            let applied = crate::types::ty_subst_applied_arguments(signature, &bindings);
            crate::trace_compiler!(
                "callable_shape",
                "classifier callable current={current:?} formals={:?} signature={signature:?} bindings={bindings:?} applied={applied:?}",
                classifier.type_params,
            );
            if !signatures.contains(&applied) {
                signatures.push(applied);
            }
        }
        queue.extend(
            direct_supertypes(source, current)
                .into_iter()
                .map(Ty::non_null),
        );
    }
    signatures
}

/// One callable shape for consumers whose declaration is known to represent exactly one function
/// type. Expected-shape consumers must use [`classifier_callable_signatures`] instead.
pub(crate) fn classifier_callable_signature(source: &dyn SymbolSource, ty: Ty) -> Option<Ty> {
    classifier_callable_signatures(source, ty)
        .into_iter()
        .next()
}

/// Normalize an applied Kotlin function classifier (`Function0<R>`, `Function2<A, B, R>`, …) to
/// its semantic function type. Callable nominal classifiers remain nominal even when their
/// hierarchy supplies an `invoke` signature.
pub(crate) fn declared_function_type(source: &dyn SymbolSource, ty: Ty) -> Option<Ty> {
    let internal = ty.non_null().kotlin_class_internal()?;
    source
        .classifier(internal)?
        .represents_function_type()
        .then(|| classifier_callable_signature(source, ty))
        .flatten()
        .map(|function| {
            if ty.is_nullable() {
                Ty::nullable(function)
            } else {
                function
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{Callables, LibraryType, ResolvedSymbols, TypeKind};
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::{type_name, TypeNameList};

    struct Shapes {
        declarations: Vec<(TypeName, std::sync::Arc<LibraryType>)>,
    }

    impl SymbolSource for Shapes {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
            let identity = namespace.existing_classifier(name);
            let classifier = identity.and_then(|identity| {
                self.declarations
                    .iter()
                    .find(|(candidate, _)| *candidate == identity)
                    .map(|(_, declaration)| declaration.clone())
            });
            std::rc::Rc::new(ResolvedSymbols {
                classifier_name: classifier.as_ref().and(identity),
                classifier,
                callables: Callables::None,
                importable_declaration: false,
            })
        }
    }

    fn callable_classifier(
        direct_supertypes: impl IntoIterator<Item = TypeName>,
    ) -> std::sync::Arc<LibraryType> {
        std::sync::Arc::new(LibraryType {
            is_kotlin: true,
            access: crate::libraries::ClassifierAccess::Public,
            source_file: None,
            stable_declaration: None,
            is_nested: false,
            outer_instance: None,
            kind: TypeKind::Interface,
            inheritance: Default::default(),
            supertypes: direct_supertypes.into_iter().collect::<Vec<_>>().into(),
            supertype_templates: Vec::new(),
            constructors: Vec::new(),
            hidden_member_properties: Default::default(),
            declared_callables: Default::default(),
            declared_callable_order: Vec::new(),
            members: Vec::new(),
            companion: Vec::new(),
            constants: Default::default(),
            sam_eligible: false,
            callable_signature: Some(Ty::fun(Vec::new(), Ty::String)),
            callable_signatures: vec![Ty::fun(Vec::new(), Ty::String)],
            companion_object: None,
            value_underlying: None,
            value_underlying_property: None,
            alias_target: None,
            type_parameters: Default::default(),
            own_type_parameter_count: 0,
            sealed_subclasses: TypeNameList::new(),
            enum_entries: Vec::new(),
            enum_entries_accessor: None,
            named_parameter_lists: Vec::new(),
            retention: None,
            annotation_targets: None,
        })
    }

    #[test]
    fn only_function_classifier_representations_normalize_to_function_types() {
        let function = type_name("fixture/Function0");
        let callable_object = type_name("fixture/CallableObject");
        let source = Shapes {
            declarations: vec![
                (
                    function,
                    callable_classifier([type_name("kotlin/Function")]),
                ),
                (callable_object, callable_classifier([function])),
            ],
        };

        let expected = Ty::fun(Vec::new(), Ty::String);
        assert_eq!(
            declared_function_type(&source, Ty::obj_name(function)),
            Some(expected)
        );
        assert_eq!(
            classifier_callable_signature(&source, Ty::obj_name(callable_object)),
            Some(expected),
            "callable objects still expose invoke shape"
        );
        assert_eq!(
            declared_function_type(&source, Ty::obj_name(callable_object)),
            None,
            "callable objects must retain nominal member scope"
        );
    }

    #[test]
    fn applied_callable_classifier_preserves_nullable_arguments() {
        let callable = type_name("fixture/Callable1");
        let parameter = "P".to_string();
        let mut declaration = (*callable_classifier([])).clone();
        declaration.type_parameters = crate::types::TypeParameters::new(
            vec![parameter.clone()],
            vec![vec![Ty::obj("kotlin/Any")]],
            vec![crate::types::TypeVariance::In],
        );
        declaration.callable_signature = Some(Ty::fun(
            vec![Ty::ty_param(&parameter, Ty::obj("kotlin/Any"))],
            Ty::Unit,
        ));
        declaration.callable_signatures = vec![declaration.callable_signature.unwrap()];
        let source = Shapes {
            declarations: vec![(callable, std::sync::Arc::new(declaration))],
        };
        assert_eq!(
            classifier_callable_signature(
                &source,
                Ty::obj_args_name(callable, &[Ty::nullable(Ty::String)]),
            ),
            Some(Ty::fun(vec![Ty::nullable(Ty::String)], Ty::Unit)),
        );
    }

    #[test]
    fn classifier_preserves_multiple_inherited_callable_shapes() {
        let zero = type_name("fixture/Function0");
        let one = type_name("fixture/Function1");
        let both = type_name("fixture/Both");
        let mut one_declaration = (*callable_classifier([])).clone();
        one_declaration.callable_signature = Some(Ty::fun(vec![Ty::Int], Ty::String));
        one_declaration.callable_signatures = vec![one_declaration.callable_signature.unwrap()];
        let mut both_declaration = (*callable_classifier([zero, one])).clone();
        both_declaration.callable_signature = None;
        both_declaration.callable_signatures.clear();
        let source = Shapes {
            declarations: vec![
                (zero, callable_classifier([])),
                (one, std::sync::Arc::new(one_declaration)),
                (both, std::sync::Arc::new(both_declaration)),
            ],
        };

        let signatures = classifier_callable_signatures(&source, Ty::obj_name(both));
        assert_eq!(signatures.len(), 2);
        assert!(signatures.contains(&Ty::fun(Vec::new(), Ty::String)));
        assert!(signatures.contains(&Ty::fun(vec![Ty::Int], Ty::String)));
    }
}
