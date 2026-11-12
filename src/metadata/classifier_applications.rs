//! Checked declaration facts used while metadata splits a semantic type application.
//!
//! A semantic application keeps an inner classifier's own type arguments first and the captured
//! outer classes' arguments after them. Lexical captures are a distinct suffix, not declarations
//! of the local classifier. Metadata consumes those boundaries without changing semantic types.

use crate::backend::BackendClassifierSource;
use crate::ir::IrFile;
use crate::metadata::{ClassifierApplication, InnerClassifier};
use crate::types::TypeName;

/// The split authenticated by the IR declaration or frozen classifier facts. A missing nested
/// declaration is an error; an ordinary top-level classifier needs no captured-layout fact.
pub fn classifier_application(
    ir: &IrFile,
    symbols: &dyn BackendClassifierSource,
    classifier: TypeName,
) -> Result<ClassifierApplication, TypeName> {
    if let Some(class) = ir
        .classes
        .iter()
        .find(|candidate| candidate.fq_name == classifier)
    {
        return if class.is_inner_class {
            classifier
                .nested_owner()
                .map(|outer| {
                    ClassifierApplication::Inner(InnerClassifier {
                        outer,
                        own_arguments: class.type_params.len(),
                    })
                })
                .ok_or(classifier)
        } else if !class.captured_type_params.is_empty() {
            Ok(ClassifierApplication::LexicalCapture {
                own_arguments: class.type_params.len(),
                captured_arguments: class.captured_type_params.len(),
            })
        } else {
            Ok(ClassifierApplication::Direct)
        };
    }
    let Some(fact) = symbols.classifier(classifier) else {
        return if classifier.nested_owner().is_none() {
            Ok(ClassifierApplication::Direct)
        } else {
            Err(classifier)
        };
    };
    if let Some(outer) = fact.outer_instance {
        return Ok(ClassifierApplication::Inner(InnerClassifier {
            outer,
            own_arguments: fact.own_type_parameter_count,
        }));
    }
    let captured_arguments = fact
        .type_param_variances
        .len()
        .checked_sub(fact.own_type_parameter_count)
        .ok_or(classifier)?;
    Ok(if captured_arguments == 0 {
        ClassifierApplication::Direct
    } else {
        ClassifierApplication::LexicalCapture {
            own_arguments: fact.own_type_parameter_count,
            captured_arguments,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrClass;
    use crate::metadata::type_encoder::{
        encode_type, StringTable, TypeEncodeError, TypeParameters,
    };
    use crate::types::Ty;

    struct NoClassifierFacts;

    impl BackendClassifierSource for NoClassifierFacts {
        fn classifier(
            &self,
            _: TypeName,
        ) -> Option<std::sync::Arc<crate::backend::BackendClassifierFact>> {
            None
        }
    }

    #[test]
    fn a_top_level_classifier_needs_no_inner_class_fact() {
        let ir = IrFile::default();
        let classifier = crate::types::type_name("kotlin/coroutines/Continuation");

        assert_eq!(
            classifier_application(&ir, &NoClassifierFacts, classifier),
            Ok(ClassifierApplication::Direct)
        );
    }

    #[test]
    fn a_nested_classifier_still_requires_a_checked_fact() {
        let ir = IrFile::default();
        let classifier = crate::types::type_name("sample/Outer").nested_child("Nested");

        assert_eq!(
            classifier_application(&ir, &NoClassifierFacts, classifier),
            Err(classifier)
        );
    }

    #[test]
    fn a_local_declaration_keeps_own_and_lexical_formals_separate() {
        let mut class = crate::ir::test_support::blank_class("sample/Local");
        class.is_local_class = true;
        class.type_params = vec!["U".into()];
        class.captured_type_params = vec!["T".into()];
        let classifier = class.fq_name;
        let mut ir = IrFile::default();
        ir.classes.push(class);

        assert_eq!(
            classifier_application(&ir, &NoClassifierFacts, classifier),
            Ok(ClassifierApplication::LexicalCapture {
                own_arguments: 1,
                captured_arguments: 1,
            })
        );
    }

    struct DependencyFacts(
        TypeName,
        std::sync::Arc<crate::backend::BackendClassifierFact>,
    );

    impl BackendClassifierSource for DependencyFacts {
        fn classifier(
            &self,
            classifier: TypeName,
        ) -> Option<std::sync::Arc<crate::backend::BackendClassifierFact>> {
            (classifier == self.0).then(|| self.1.clone())
        }
    }

    fn dependency_fact(
        outer_instance: Option<TypeName>,
        own_type_parameter_count: usize,
        semantic_type_parameter_count: usize,
    ) -> std::sync::Arc<crate::backend::BackendClassifierFact> {
        std::sync::Arc::new(crate::backend::BackendClassifierFact {
            runtime_exception: None,
            declaration_name: None,
            access: crate::libraries::ClassifierAccess::Public,
            is_kotlin: true,
            mapped_builtin: false,
            source: false,
            outer_instance,
            kind: crate::libraries::TypeKind::Class,
            is_abstract: false,
            is_extensible: false,
            supertypes: Box::default(),
            annotations: Box::default(),
            own_type_parameter_count,
            type_param_variances: vec![
                crate::types::TypeVariance::Invariant;
                semantic_type_parameter_count
            ]
            .into_boxed_slice(),
            value_underlying: None,
            value_declaration: None,
            role: None,
            companion: None,
            enum_entries_accessor: None,
        })
    }

    #[test]
    fn a_dependency_classifier_splits_its_application_by_its_published_layout() {
        let ir = IrFile::default();
        let nested = crate::types::type_name("sample/Outer").nested_child("Nested");
        let outer = crate::types::type_name("sample/Outer");
        for (fact, expected) in [
            (
                dependency_fact(None, 1, 3),
                Ok(ClassifierApplication::LexicalCapture {
                    own_arguments: 1,
                    captured_arguments: 2,
                }),
            ),
            (
                dependency_fact(Some(outer), 1, 2),
                Ok(ClassifierApplication::Inner(InnerClassifier {
                    outer,
                    own_arguments: 1,
                })),
            ),
            (
                dependency_fact(None, 2, 2),
                Ok(ClassifierApplication::Direct),
            ),
            (dependency_fact(None, 2, 1), Err(nested)),
        ] {
            assert_eq!(
                classifier_application(&ir, &DependencyFacts(nested, fact), nested),
                expected
            );
        }
    }

    #[test]
    fn lexical_capture_omission_preserves_the_complete_own_application() {
        let local = crate::types::type_name("sample/Local");
        let locals = std::iter::once(local).collect();
        let lexical = |classifier| {
            Ok(if classifier == local {
                ClassifierApplication::LexicalCapture {
                    own_arguments: 1,
                    captured_arguments: 1,
                }
            } else {
                ClassifierApplication::Direct
            })
        };
        let direct = |_| Ok(ClassifierApplication::Direct);
        let mut actual = StringTable::with_local_classifiers(
            &locals,
            &Default::default(),
            &Default::default(),
            None,
            &lexical,
        );
        let mut expected = StringTable::with_local_classifiers(
            &locals,
            &Default::default(),
            &Default::default(),
            None,
            &direct,
        );
        let actual_type = encode_type(
            &mut actual,
            Ty::obj_args_name(local, &[Ty::Int, Ty::String]),
            &TypeParameters::new(),
        )
        .unwrap();
        let expected_type = encode_type(
            &mut expected,
            Ty::obj_args_name(local, &[Ty::Int]),
            &TypeParameters::new(),
        )
        .unwrap();
        assert_eq!(actual_type.as_bytes(), expected_type.as_bytes());
        assert_eq!(
            actual.serialize_types().as_bytes(),
            expected.serialize_types().as_bytes()
        );
        assert_eq!(actual.into_strings(), expected.into_strings());
    }

    #[test]
    fn a_lexical_application_requires_its_complete_capture_frame() {
        let local = crate::types::type_name("sample/Local");
        let lexical = |_| {
            Ok(ClassifierApplication::LexicalCapture {
                own_arguments: 1,
                captured_arguments: 1,
            })
        };
        for count in [0, 1, 3] {
            let mut strings = StringTable::with_local_classifiers(
                &Default::default(),
                &Default::default(),
                &Default::default(),
                None,
                &lexical,
            );
            let Err(error) = encode_type(
                &mut strings,
                Ty::obj_args_name(local, &vec![Ty::Int; count]),
                &TypeParameters::new(),
            ) else {
                panic!("an incomplete lexical capture frame was encoded")
            };
            assert_eq!(
                error,
                TypeEncodeError::LexicalClassifierArity {
                    classifier: local,
                    arguments: count,
                    own: 1,
                    captured: 1,
                }
            );
        }
    }

    fn local_class(name: &str, own: &[&str], captured: &[&str], anonymous: bool) -> IrFile {
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(crate::types::type_name(name));
        class.is_anonymous_object = anonymous;
        class.is_local_class = !anonymous;
        class.type_params = own.iter().map(|name| (*name).to_owned()).collect();
        class.captured_type_params = captured.iter().map(|name| (*name).to_owned()).collect();
        ir.classes.push(class);
        ir
    }

    #[test]
    fn an_anonymous_factory_owns_no_arguments_and_has_no_outer_type() {
        let ir = local_class("app/FileKt$copy$1", &[], &["T"], true);
        let classifier = ir.classes[0].fq_name;

        assert_eq!(
            classifier_application(&ir, &NoClassifierFacts, classifier),
            Ok(ClassifierApplication::LexicalCapture {
                own_arguments: 0,
                captured_arguments: 1,
            })
        );
    }

    #[test]
    fn a_named_local_class_keeps_its_own_arguments_apart_from_enclosing_captures() {
        let ir = local_class("app/FileKt$enclose$Holder", &["U"], &["T"], false);
        let classifier = ir.classes[0].fq_name;

        assert_eq!(
            classifier_application(&ir, &NoClassifierFacts, classifier),
            Ok(ClassifierApplication::LexicalCapture {
                own_arguments: 1,
                captured_arguments: 1,
            })
        );
    }

    #[test]
    fn a_local_class_without_captures_uses_every_argument() {
        let ir = local_class("app/FileKt$enclose$Holder", &["U"], &[], false);
        let classifier = ir.classes[0].fq_name;

        assert_eq!(
            classifier_application(&ir, &NoClassifierFacts, classifier),
            Ok(ClassifierApplication::Direct)
        );
    }

    #[test]
    fn an_inner_class_still_names_its_enclosing_instance() {
        let outer = crate::types::type_name("sample/Outer");
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(outer.nested_child("Inner"));
        class.is_inner_class = true;
        class.type_params = vec!["U".to_owned()];
        class.captured_type_params = vec!["T".to_owned()];
        ir.classes.push(class);
        let classifier = ir.classes[0].fq_name;

        assert_eq!(
            classifier_application(&ir, &NoClassifierFacts, classifier),
            Ok(ClassifierApplication::Inner(InnerClassifier {
                outer,
                own_arguments: 1,
            }))
        );
    }
}
