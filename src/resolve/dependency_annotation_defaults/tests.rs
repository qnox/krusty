use super::*;
use crate::libraries::{
    AnnotationElementDefault, ParamFlags, ParamList, ResolvedSymbols, TypeKind,
};
use crate::symbol_source::SymbolNamespace;
use crate::types::{type_name, WrittenAnnotation};
use std::{rc::Rc, sync::Arc};

struct AnnotationSource;

impl SymbolSource for AnnotationSource {
    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
        let classifier = type_name("Inner");
        if namespace.existing_classifier(name) != Some(classifier) {
            return Rc::new(ResolvedSymbols::default());
        }
        let mut shape = LibraryType::declaration_header();
        shape.kind = TypeKind::Annotation;
        shape.named_parameter_lists.push(ParamList {
            visibility: crate::types::Visibility::Public,
            names: ["fallback", "first", "second", "rest"]
                .map(str::to_owned)
                .to_vec(),
            defaults: [true, false, false, false].into_iter().collect(),
            types: vec![Ty::Int, Ty::Int, Ty::Int, Ty::array(Ty::Int)],
            recv_fun: ParamFlags::new(),
            vararg: Some(3),
            annotation: None,
        });
        shape.annotation_element_defaults.push((
            "fallback".into(),
            AnnotationElementDefault::Evaluated(AnnotationValue::Int(9)),
        ));
        Rc::new(ResolvedSymbols {
            classifier_name: Some(classifier),
            classifier: Some(Arc::new(shape)),
            ..ResolvedSymbols::default()
        })
    }
}

fn payload(written: Option<Box<WrittenAnnotation>>) -> AnnotationValue {
    AnnotationValue::Annotation {
        internal: type_name("Inner"),
        values: vec![
            ("first".into(), AnnotationValue::Int(1)),
            ("second".into(), AnnotationValue::Int(2)),
            ("rest".into(), AnnotationValue::Array(Vec::new())),
        ],
        written,
    }
}

#[test]
fn sparse_provider_membership_survives_runtime_default_filling() {
    let value = element_default(&AnnotationSource, &payload(None), Ty::obj("Inner")).unwrap();
    assert_eq!(
        value,
        DefaultValue::Annotation {
            classifier: type_name("Inner"),
            members: vec![
                ("fallback".into(), Ty::Int),
                ("first".into(), Ty::Int),
                ("second".into(), Ty::Int),
                ("rest".into(), Ty::array(Ty::Int)),
            ],
            values: vec![
                DefaultValue::Int(9),
                DefaultValue::Int(1),
                DefaultValue::Int(2),
                DefaultValue::Array {
                    array_type: Ty::array(Ty::Int),
                    elements: Vec::new()
                },
            ],
            arguments: AnnotationArgumentProvenance::checked(4, None, vec![0]).unwrap(),
        }
    );
}

#[test]
fn provider_written_indices_map_through_sparse_members_and_reject_invalid_payloads() {
    let written = WrittenAnnotation {
        short_name: "Inner".into(),
        use_site: None,
        order: Box::new([1, 0]),
    };
    let value = element_default(
        &AnnotationSource,
        &payload(Some(Box::new(written.clone()))),
        Ty::obj("Inner"),
    )
    .unwrap();
    let DefaultValue::Annotation { arguments, .. } = value else {
        panic!("annotation default")
    };
    assert_eq!(
        arguments,
        AnnotationArgumentProvenance::checked(4, Some(vec![2, 1]), vec![0]).unwrap()
    );
    for order in [Box::new([0, 0]), Box::new([3, 0])] {
        let invalid = WrittenAnnotation {
            order,
            ..written.clone()
        };
        assert_eq!(
            element_default(
                &AnnotationSource,
                &payload(Some(Box::new(invalid))),
                Ty::obj("Inner")
            ),
            None
        );
    }
    let mut duplicate = payload(None);
    let AnnotationValue::Annotation { values, .. } = &mut duplicate else {
        unreachable!()
    };
    values.push(values[0].clone());
    assert_eq!(
        element_default(&AnnotationSource, &duplicate, Ty::obj("Inner")),
        None
    );
}
