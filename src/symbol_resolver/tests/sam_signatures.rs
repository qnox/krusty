use super::*;

fn sam_classifier(
    formals: &[&str],
    supertypes: Vec<Ty>,
    members: Vec<LibraryMember>,
) -> LibraryType {
    let mut classifier = fake_library_type(Vec::new(), Vec::new());
    classifier.kind = TypeKind::Interface;
    classifier.sam_eligible = true;
    classifier.type_parameters = crate::types::TypeParameters::invariant(
        formals.iter().map(|formal| (*formal).to_string()).collect(),
        vec![Vec::new(); formals.len()],
    );
    classifier.own_type_parameter_count = formals.len();
    classifier.supertypes = supertypes
        .iter()
        .filter_map(|supertype| supertype.obj_internal())
        .collect::<Vec<_>>()
        .into();
    classifier.supertype_templates = supertypes;
    classifier.members = members;
    classifier
}

fn abstract_generic_member(name: &str, params: Vec<Ty>, ret: Ty) -> LibraryMember {
    let mut member = LibraryMember::new(name.to_string(), params.clone(), ret, String::new());
    member.generic_sig = Some(GenericSig {
        formals: Vec::new(),
        formal_bounds: Vec::new(),
        receiver: None,
        params,
        ret,
        return_policy: Default::default(),
    });
    member.set_is_abstract(true);
    member
}

#[test]
fn sam_signature_uses_the_applied_inherited_declaration() {
    let bound = Ty::nullable(Ty::obj("kotlin/Any"));
    let input = Ty::ty_param("I", bound);
    let output = Ty::ty_param("O", bound);
    let parameter = Ty::ty_param("T", bound);
    let base = sam_classifier(
        &["I", "O"],
        Vec::new(),
        vec![abstract_generic_member("apply", vec![input], output)],
    );
    let operation = sam_classifier(
        &["T"],
        vec![Ty::obj_args("test/Base", &[parameter, parameter])],
        Vec::new(),
    );
    let source = SamHierarchySource {
        classifiers: [
            (
                crate::types::type_name("test/Base"),
                std::sync::Arc::new(base),
            ),
            (
                crate::types::type_name("test/Operation"),
                std::sync::Arc::new(operation),
            ),
        ]
        .into_iter()
        .collect(),
    };

    let signature = semantic_sam_signature(&source, Ty::obj_args("test/Operation", &[Ty::String]))
        .expect("the inherited abstract declaration is the SAM");
    assert_eq!(
        signature.internal,
        crate::types::type_name("test/Operation")
    );
    assert_eq!(signature.method, "apply");
    assert_eq!(signature.params, [Ty::String]);
    assert_eq!(signature.ret, Ty::String);
}

#[test]
fn sam_signature_rejects_two_distinct_abstract_declarations() {
    let left = sam_classifier(
        &[],
        Vec::new(),
        vec![abstract_generic_member("left", Vec::new(), Ty::Unit)],
    );
    let right = sam_classifier(
        &[],
        Vec::new(),
        vec![abstract_generic_member("right", Vec::new(), Ty::Unit)],
    );
    let both = sam_classifier(
        &[],
        vec![Ty::obj("test/Left"), Ty::obj("test/Right")],
        Vec::new(),
    );
    let source = SamHierarchySource {
        classifiers: [
            (
                crate::types::type_name("test/Left"),
                std::sync::Arc::new(left),
            ),
            (
                crate::types::type_name("test/Right"),
                std::sync::Arc::new(right),
            ),
            (
                crate::types::type_name("test/Both"),
                std::sync::Arc::new(both),
            ),
        ]
        .into_iter()
        .collect(),
    };

    assert!(semantic_sam_signature(&source, Ty::obj("test/Both")).is_none());
}

#[test]
fn sam_signature_merges_platform_flexible_override_slot() {
    let kotlin = sam_classifier(
        &[],
        Vec::new(),
        vec![abstract_generic_member("get", vec![Ty::Int], Ty::Int)],
    );
    let mut java = sam_classifier(
        &[],
        Vec::new(),
        vec![abstract_generic_member(
            "get",
            vec![Ty::platform_nullable(Ty::Int)],
            Ty::platform_nullable(Ty::Int),
        )],
    );
    java.is_kotlin = false;
    let mixed = sam_classifier(
        &[],
        vec![Ty::obj("test/KotlinGet"), Ty::obj("test/JavaGet")],
        vec![abstract_generic_member("get", vec![Ty::Int], Ty::Int)],
    );
    let source = SamHierarchySource {
        classifiers: [
            (
                crate::types::type_name("test/KotlinGet"),
                std::sync::Arc::new(kotlin),
            ),
            (
                crate::types::type_name("test/JavaGet"),
                std::sync::Arc::new(java),
            ),
            (
                crate::types::type_name("test/MixedGet"),
                std::sync::Arc::new(mixed),
            ),
        ]
        .into_iter()
        .collect(),
    };

    let signature = semantic_sam_signature(&source, Ty::obj("test/MixedGet"))
        .expect("the direct Kotlin override occupies the inherited platform method slot");
    assert_eq!(signature.method, "get");
    assert_eq!(signature.params, [Ty::Int]);
    assert_eq!(signature.ret, Ty::Int);
}

#[test]
fn sam_signature_keeps_an_explicit_abstract_object_method() {
    let some_fun = sam_classifier(
        &[],
        Vec::new(),
        vec![abstract_generic_member("toString", Vec::new(), Ty::String)],
    );
    let source = SamHierarchySource {
        classifiers: [(
            crate::types::type_name("test/SomeFun"),
            std::sync::Arc::new(some_fun),
        )]
        .into_iter()
        .collect(),
    };

    let signature = semantic_sam_signature(&source, Ty::obj("test/SomeFun"))
        .expect("the directly redeclared abstract method is the SAM");
    assert_eq!(signature.method, "toString");
    assert!(signature.params.is_empty());
    assert_eq!(signature.ret, Ty::String);
}

#[test]
fn java_sam_ignores_an_explicit_abstract_object_method() {
    let mut comparator = sam_classifier(
        &["T"],
        Vec::new(),
        vec![
            abstract_generic_member("equals", vec![Ty::obj("java/lang/Object")], Ty::Boolean),
            abstract_generic_member(
                "compare",
                vec![
                    Ty::ty_param("T", Ty::obj("kotlin/Any")),
                    Ty::ty_param("T", Ty::obj("kotlin/Any")),
                ],
                Ty::Int,
            ),
        ],
    );
    comparator.is_kotlin = false;
    let source = SamHierarchySource {
        classifiers: [(
            crate::types::type_name("test/Comparator"),
            std::sync::Arc::new(comparator),
        )]
        .into_iter()
        .collect(),
    };

    let signature = semantic_sam_signature(&source, Ty::obj_args("test/Comparator", &[Ty::String]))
        .expect("Object-shaped Java declarations do not count against SAM eligibility");
    assert_eq!(signature.method, "compare");
    assert_eq!(signature.params, [Ty::String, Ty::String]);
    assert_eq!(signature.ret, Ty::Int);
}

#[test]
fn generic_constraint_collection_projects_callable_references_through_sam() {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let input = Ty::ty_param("I", any);
    let output = Ty::ty_param("O", any);
    let function = sam_classifier(
        &["I", "O"],
        Vec::new(),
        vec![abstract_generic_member("apply", vec![input], output)],
    );
    let source = SamHierarchySource {
        classifiers: [(
            crate::types::type_name("test/Function"),
            std::sync::Arc::new(function),
        )]
        .into_iter()
        .collect(),
    };
    let t = Ty::ty_param("T", any);
    let u = Ty::ty_param("U", any);
    let signature = GenericSig {
        formals: vec!["T".to_string(), "U".to_string()],
        formal_bounds: vec![vec![any], vec![any]],
        receiver: None,
        params: vec![Ty::obj_args(
            "test/Function",
            &[Ty::in_projection(t), Ty::out_projection(u)],
        )],
        ret: Ty::obj_args("test/Result", &[t]),
        return_policy: GenericReturnPolicy::Exact,
    };
    let actual = Ty::fun(vec![Ty::String], Ty::Boolean);

    let constrained = infer_generic_call_constraints_from_symbols(
        &source,
        &signature,
        [(0, actual, false)],
        None,
    );
    let mut complete = constrained.bindings.clone();
    complete.extend(constrained.tightest_upper_bindings(&source));
    assert_eq!(complete.get("T"), Some(&Ty::String));
    assert_eq!(complete.get("U"), Some(&Ty::Boolean));

    let bindings =
        infer_generic_call_bindings_from_symbols(&source, &signature, [(0, actual, false)], None);
    assert_eq!(bindings.get("T"), Some(&Ty::String));
    assert_eq!(bindings.get("U"), Some(&Ty::Boolean));
}
