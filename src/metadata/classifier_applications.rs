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
pub(crate) fn classifier_application(
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
        let mut actual =
            StringTable::with_local_classifiers(&locals, &Default::default(), None, &lexical);
        let mut expected =
            StringTable::with_local_classifiers(&locals, &Default::default(), None, &direct);
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
}
