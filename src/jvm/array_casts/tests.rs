use super::*;
use crate::backend::BackendClassifierFact;
use crate::fx_hash::HashMap;
use crate::types::{type_name, TypeVariance};
use std::sync::Arc;

#[derive(Default)]
struct Facts(HashMap<TypeName, Arc<BackendClassifierFact>>);

impl BackendClassifierSource for Facts {
    fn classifier(&self, name: TypeName) -> Option<Arc<BackendClassifierFact>> {
        self.0.get(&name).cloned()
    }
}

fn hierarchy(edges: &[(&str, &[&str])]) -> Facts {
    let mut facts = Facts::default();
    for (name, parents) in edges {
        facts.0.insert(
            type_name(name),
            Arc::new(BackendClassifierFact {
                runtime_exception: None,
                declaration_name: None,
                access: crate::libraries::ClassifierAccess::Public,
                is_kotlin: true,
                mapped_builtin: false,
                source: true,
                outer_instance: None,
                kind: crate::libraries::TypeKind::Class,
                is_abstract: false,
                is_extensible: true,
                supertypes: parents.iter().map(|name| type_name(name)).collect(),
                annotations: Box::new([]),
                own_type_parameter_count: 0,
                type_param_variances: Box::new([]),
                value_underlying: None,
                value_declaration: None,
                role: None,
                companion: None,
                enum_entries_accessor: None,
            }),
        );
    }
    facts
}

fn array(element: Ty) -> Ty {
    Ty::obj_args_name(wk::array(), &[element])
}

fn parameter(name: &str, bounds: &[(Ty, bool)]) -> IrTypeParameter {
    IrTypeParameter {
        name: "T".into(),
        semantic_name: name.into(),
        bounds: bounds.to_vec(),
        variance: TypeVariance::Invariant,
        reified: false,
    }
}

#[test]
fn peels_only_matched_reference_arrays_and_uses_readable_projections() {
    let ir = IrFile::default();
    let facts = Facts::default();
    let classifiers = CheckedClassifiers::new(&ir, &facts);
    let parameters = TypeParameters(Vec::new());
    let string = Ty::String;
    let any = Ty::obj_name(wk::any());
    for (actual, expected, result) in [
        (array(string), array(string), true),
        (
            Ty::nullable(array(Ty::nullable(string))),
            array(string),
            true,
        ),
        (array(array(string)), array(array(string)), true),
        (array(array(string)), array(any), true),
        (array(Ty::out_projection(string)), array(string), true),
        (array(Ty::in_projection(string)), array(any), true),
        (array(Ty::in_projection(string)), array(string), false),
        (Ty::array(Ty::Int), array(Ty::Int), false),
        (Ty::array(Ty::Int), Ty::array(Ty::Int), false),
        (any, array(string), false),
        (string, string, false),
        (array(any), array(string), false),
        (array(Ty::Error), array(any), false),
    ] {
        assert_eq!(
            compatible(actual, expected, &parameters, &classifiers),
            result,
            "{actual:?} as {expected:?}"
        );
    }
}

#[test]
fn uses_core_semantic_hierarchy_without_jvm_aliases_or_missing_fact_recovery() {
    let ir = IrFile::default();
    let facts = hierarchy(&[
        ("sample/Leaf", &["sample/Middle"]),
        ("sample/Middle", &["sample/Base"]),
        ("sample/Base", &[]),
        ("sample/CycleA", &["sample/CycleB"]),
        ("sample/CycleB", &["sample/CycleA"]),
        ("sample/Broken", &["sample/Missing"]),
    ]);
    let classifiers = CheckedClassifiers::new(&ir, &facts);
    let parameters = TypeParameters(Vec::new());
    for (actual, expected, result) in [
        ("sample/Leaf", "sample/Base", true),
        ("sample/Base", "sample/Leaf", false),
        ("sample/CycleA", "sample/Base", false),
        ("sample/Broken", "sample/Base", false),
        ("kotlin/collections/MutableList", "java/util/List", false),
    ] {
        assert_eq!(
            compatible(
                array(Ty::obj(actual)),
                array(Ty::obj(expected)),
                &parameters,
                &classifiers
            ),
            result
        );
    }
}

#[test]
fn expected_parameter_uses_primary_class_but_actual_parameter_uses_all_bounds() {
    let ir = IrFile::default();
    let facts = hierarchy(&[("sample/Base", &[]), ("sample/Marker", &[])]);
    let classifiers = CheckedClassifiers::new(&ir, &facts);
    let base = Ty::obj("sample/Base");
    let marker = Ty::obj("sample/Marker");
    let expected = parameter("expected#T", &[(marker, true), (base, false)]);
    let actual = parameter("actual#T", &[(base, false), (marker, true)]);
    let chained = parameter("chain#T", &[(expected.ty(), false)]);
    let parameters = TypeParameters(vec![&expected, &actual, &chained]);
    for (source, target, result) in [
        (base, expected.ty(), true),
        (marker, expected.ty(), false),
        (actual.ty(), marker, true),
        (base, chained.ty(), true),
    ] {
        assert_eq!(
            compatible(array(source), array(target), &parameters, &classifiers),
            result
        );
    }
}

#[test]
fn missing_and_cyclic_formals_never_become_an_any_proof() {
    let ir = IrFile::default();
    let facts = Facts::default();
    let classifiers = CheckedClassifiers::new(&ir, &facts);
    let any = Ty::obj_name(wk::any());
    let missing = Ty::ty_param("missing#T", any);
    let first = parameter("first#T", &[(Ty::ty_param("second#T", any), false)]);
    let second = parameter("second#T", &[(first.ty(), false)]);
    let unbounded = parameter("known#T", &[]);
    let parameters = TypeParameters(vec![&first, &second, &unbounded]);
    assert!(!compatible(
        array(Ty::String),
        array(missing),
        &parameters,
        &classifiers
    ));
    assert!(!compatible(
        array(missing),
        array(any),
        &parameters,
        &classifiers
    ));
    assert!(!compatible(
        array(Ty::String),
        array(first.ty()),
        &parameters,
        &classifiers
    ));
    assert!(!compatible(
        array(first.ty()),
        array(any),
        &parameters,
        &classifiers
    ));
    assert!(compatible(
        array(Ty::String),
        array(unbounded.ty()),
        &parameters,
        &classifiers
    ));
    assert!(compatible(
        array(unbounded.ty()),
        array(any),
        &parameters,
        &classifiers
    ));
}

#[test]
fn scratch_body_retains_caller_and_class_declarations_without_global_name_lookup() {
    let mut ir = IrFile::default();
    let facts = Facts::default();
    let caller = parameter("caller#T", &[]);
    let scratch = parameter("scratch#T", &[]);
    let class_parameter = parameter("class#T", &[]);
    let foreign_parameter = parameter("foreign#T", &[]);
    for (function, formal) in [&caller, &scratch].into_iter().enumerate() {
        ir.functions.push(crate::ir::IrFunction {
            name: "body".into(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.signatures.insert(
            function as u32,
            crate::ir::IrGenericSig {
                type_params: vec![formal.clone()],
                params: Vec::new(),
                ret: Some(Ty::Unit),
                supers: Vec::new(),
            },
        );
    }
    let owner = type_name("sample/Owner");
    ir.insert_class_signature_name(
        owner,
        crate::ir::IrGenericSig {
            type_params: vec![class_parameter.clone()],
            params: Vec::new(),
            ret: None,
            supers: Vec::new(),
        },
    );
    ir.record_foreign_template_classifier(
        type_name("dependency/Owner"),
        vec![foreign_parameter.clone()],
    );
    let parameters = TypeParameters::at(
        &ir,
        [Some(0), Some(1)],
        Some(StaticOwner::Class(owner)),
        &facts,
    );
    assert_eq!(
        parameters
            .named("caller#T")
            .map(|p| p.semantic_name.as_str()),
        Some("caller#T")
    );
    assert_eq!(
        parameters
            .named("scratch#T")
            .map(|p| p.semantic_name.as_str()),
        Some("scratch#T")
    );
    assert_eq!(
        parameters
            .named("class#T")
            .map(|p| p.semantic_name.as_str()),
        Some("class#T")
    );
    assert!(parameters.named("foreign#T").is_none());
    let constructor =
        TypeParameters::at(&ir, [None, None], Some(StaticOwner::Class(owner)), &facts);
    assert!(constructor.named("class#T").is_some());
    assert!(constructor.named("caller#T").is_none());
    let foreign = TypeParameters::at(
        &ir,
        [None, None],
        Some(StaticOwner::Class(type_name("dependency/Owner"))),
        &facts,
    );
    assert_eq!(
        foreign.named("foreign#T").map(|p| p.semantic_name.as_str()),
        Some("foreign#T")
    );
}
