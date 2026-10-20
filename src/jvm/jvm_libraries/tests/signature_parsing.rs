use super::*;

#[test]
fn method_descriptor_parser_rejects_partial_or_malformed_input() {
    assert_eq!(
        parse_method_desc("(ILjava/lang/String;)[B"),
        Some((vec![Ty::Int, Ty::String], Ty::array(Ty::Byte),))
    );
    for invalid in [
        "",
        "I)V",
        "(I",
        "(I)",
        "(V)V",
        "([V)V",
        "(Q)V",
        "(Ljava/lang/String)V",
        "(Ljava/lang/String;;)V",
        "()Ljava/lang/String;I",
    ] {
        assert_eq!(
            parse_method_desc(invalid),
            None,
            "accepted malformed descriptor {invalid}"
        );
    }
}

#[test]
fn method_descriptor_preserves_declared_primitive_width() {
    assert_eq!(parse_method_desc("()B"), Some((Vec::new(), Ty::Byte)));
    assert_eq!(parse_method_desc("()S"), Some((Vec::new(), Ty::Short)));
    assert_eq!(
        parse_method_desc("(BS)S"),
        Some((vec![Ty::Byte, Ty::Short], Ty::Short))
    );
}

#[test]
fn method_generic_signature_retains_interformal_bounds() {
    let signature = parse_method_gsig(
        "<T:Ljava/lang/Object;R:Ljava/lang/Object;C::Ljava/util/Collection<-TR;>;>\
             ([TT;TC;Lkotlin/jvm/functions/Function1<-TT;+TR;>;)TC;",
    )
    .expect("generic signature");
    assert_eq!(signature.formals, ["T", "R", "C"]);
    assert_eq!(
        signature.formal_bounds[2],
        [Ty::platform_nullable(Ty::obj_args(
            "kotlin/collections/Collection",
            &[Ty::in_projection(Ty::ty_param(
                "R",
                Ty::platform_nullable(Ty::obj("kotlin/Any")),
            ))],
        ))]
    );
}

#[test]
fn java_flexibility_recurses_through_formal_bounds_and_projections() {
    let (_, bounds, _) =
        parse_formals("<T:Ljava/util/List<Ljava/lang/String;>;>Ljava/lang/Object;");
    assert_eq!(
        bounds,
        [vec![Ty::platform_nullable(Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::platform_nullable(Ty::String)],
        ))]],
    );

    assert_eq!(
        java_type_nullability(
            Ty::obj_args("java/util/List", &[Ty::in_projection(Ty::String)]),
            None,
        ),
        Ty::platform_nullable(Ty::obj_args(
            "java/util/List",
            &[Ty::in_projection(Ty::platform_nullable(Ty::String))],
        )),
    );

    let base = Ty::obj("fixtures/Base");
    assert_eq!(
        java_type_nullability(Ty::array(base), None),
        Ty::platform_nullable(Ty::obj_args("kotlin/Array", &[Ty::platform_nullable(base)],)),
        "the Java provider must retain the flexible reference-array lower bound",
    );

    assert_eq!(
            java_type_nullability(Ty::fun(vec![Ty::String, Ty::Int], Ty::String), None),
            Ty::platform_nullable(Ty::fun(
                vec![Ty::platform_nullable(Ty::String), Ty::Int],
                Ty::platform_nullable(Ty::String),
            )),
            "Java publication makes function containers and reference positions flexible without boxing scalar positions",
        );
}

#[test]
fn raw_signature_marks_only_outer_method_formal_returns_flexible() {
    // The policy belongs to the exact declaration shape: a method-owned outer formal denotes an
    // unannotated reference result, while a nested occurrence already has a reference container and
    // an owner formal is not rebound by the method call. This prevents broad classpath provenance from
    // changing unrelated generic returns.
    let direct = parse_method_gsig("<T:Ljava/lang/Object;>(TT;)TT;").expect("direct formal");
    assert_eq!(direct.return_policy, GenericReturnPolicy::FlexibleReference);

    let nested = parse_method_gsig("<T:Ljava/lang/Object;>(TT;)Ljava/util/List<TT;>;")
        .expect("nested formal");
    assert_eq!(nested.return_policy, GenericReturnPolicy::Exact);

    let owner = parse_method_gsig("(TT;)TT;").expect("owner formal");
    assert_eq!(owner.return_policy, GenericReturnPolicy::Exact);
}

#[test]
fn ordinary_generic_signatures_retain_projection_and_inner_class_parsing() {
    let method = parse_method_gsig("(Ljava/util/List<+Ljava/lang/String;>;)Ljava/util/List<*>;")
        .expect("method signature");
    assert_eq!(
        method.params,
        [Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::out_projection(Ty::String)]
        )]
    );
    assert_eq!(
        method.ret,
        Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))]
        )
    );

    let (_, _, supertypes) = parse_class_gsig(
        "Ljava/lang/Object;\
             Lfixture/Outer<Ljava/lang/Integer;>.Inner<Ljava/lang/Long;>;",
    )
    .expect("class signature");
    assert_eq!(
        supertypes[1],
        Ty::obj_args("fixture/Outer$Inner", &[Ty::obj("java/lang/Long")])
    );
}

#[test]
fn class_generic_signature_canonicalizes_unsigned_arguments() {
    let (_, _, supertypes) =
        parse_class_gsig("Ljava/lang/Object;Ljava/lang/Iterable<Lkotlin/UInt;>;")
            .expect("class signature");
    assert_eq!(
        supertypes[1],
        Ty::obj_args("kotlin/collections/Iterable", &[Ty::UInt])
    );
}

#[test]
fn class_generic_signature_retains_formal_bounds() {
    let (formals, bounds, _) = parse_class_gsig("<T:Ljava/lang/CharSequence;>Ljava/lang/Object;")
        .expect("class signature");
    assert_eq!(formals, ["T"]);
    assert_eq!(
        bounds,
        [vec![Ty::platform_nullable(Ty::obj("kotlin/CharSequence"))]]
    );
}

#[test]
fn function_generic_signature_canonicalizes_bottom_and_unit_returns() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )));
    let unit = libraries.semanticize_jvm_generic_sig(
        parse_method_gsig("(Lkotlin/jvm/functions/Function0<Lkotlin/Unit;>;)V")
            .expect("Unit function signature"),
    );
    assert_eq!(unit.params, [Ty::fun(Vec::new(), Ty::Unit)]);

    let bottom = libraries.semanticize_jvm_generic_sig(
        parse_method_gsig("(Lkotlin/jvm/functions/Function0<Lkotlin/Nothing;>;)V")
            .expect("Nothing function signature"),
    );
    assert_eq!(bottom.params, [Ty::fun(Vec::new(), Ty::Nothing)]);
}

#[test]
fn function_generic_signature_consumes_function_interface_variance() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )));
    let signature = libraries.semanticize_jvm_generic_sig(
        parse_method_gsig(
            "<T:Ljava/lang/Object;>(Lkotlin/jvm/functions/Function1<\
                 -Ljava/lang/String;+TT;>;)TT;",
        )
        .expect("function-typed generic signature"),
    );
    assert_eq!(
        signature.params,
        [Ty::fun(
            vec![Ty::String],
            Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any"))),
        )]
    );
}

#[test]
fn java_function_signature_keeps_boxed_slot_flexibility_when_semanticized() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )));
    let signature = libraries
        .publish_java_member_generic_signature(
            Some(
                "()Lkotlin/jvm/functions/Function1<\
                     Ljava/lang/String;Ljava/lang/Integer;>;",
            ),
            &Default::default(),
            &[],
            None,
            None,
        )
        .expect("Java function-typed generic signature");
    assert_eq!(
        signature.ret,
        Ty::platform_nullable(Ty::fun(
            vec![Ty::platform_nullable(Ty::String)],
            Ty::platform_nullable(Ty::Int),
        )),
        "Java reference flexibility must be recorded before Function1 slots are unboxed",
    );
}

#[test]
fn field_generic_signature_must_be_complete_and_concrete() {
    assert_eq!(
        parse_concrete_field_gsig(
            "Ljava/util/List<Lkotlin/collections/Set<Ljava/lang/Integer;>;>;",
            "Ljava/util/List;",
        ),
        Some(Ty::obj_args(
            "kotlin/collections/List",
            &[Ty::obj_args("kotlin/collections/Set", &[Ty::Int])],
        ))
    );
    assert_eq!(
        parse_concrete_field_gsig(
            "Ljava/util/List<Ljava/lang/String;>;junk",
            "Ljava/util/List;",
        ),
        None
    );
    assert_eq!(
        parse_concrete_field_gsig("Ljava/util/List<Ljava/lang/String;", "Ljava/util/List;",),
        None
    );
}

#[test]
fn field_generic_signature_rejects_wildcards_and_projections() {
    assert_eq!(
        parse_field_gsig(
            "TT;",
            "Ljava/lang/CharSequence;",
            &[(
                "T".to_string(),
                Ty::platform_nullable(Ty::obj("kotlin/CharSequence")),
            )]
            .into_iter()
            .collect(),
        )
        .map(|(ty, _)| ty),
        Some(Ty::ty_param(
            "T",
            Ty::platform_nullable(Ty::obj("kotlin/CharSequence"))
        )),
        "a field type variable must erase through, and carry, its declaring-class bound"
    );
    for signature in [
        "Ljava/util/List<*>;",
        "Ljava/util/List<+Ljava/lang/String;>;",
        "Ljava/util/List<-Ljava/lang/String;>;",
        "Ljava/util/List<Ljava/util/Set<+Ljava/lang/String;>;>;",
        "Lkotlin/jvm/functions/Function1<-Ljava/lang/String;+Ljava/lang/Long;>;",
    ] {
        let descriptor = format!("L{};", &signature[1..signature.find('<').unwrap()]);
        assert_eq!(
            parse_concrete_field_gsig(signature, &descriptor),
            None,
            "accepted lossy field signature {signature}"
        );
    }
}

#[test]
fn field_generic_signature_recurses_through_function_types() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )));
    let raw = parse_concrete_field_gsig(
        "Lkotlin/jvm/functions/Function1<\
             Ljava/util/List<Ljava/lang/Integer;>;\
             Ljava/util/Set<Ljava/lang/Long;>;>;",
        "Lkotlin/jvm/functions/Function1;",
    )
    .expect("concrete generic field signature");
    assert_eq!(
        libraries.semanticize_jvm_type(raw),
        Ty::fun(
            vec![Ty::obj_args("kotlin/collections/List", &[Ty::Int])],
            Ty::obj_args("kotlin/collections/Set", &[Ty::Long]),
        )
    );
    assert_eq!(
        parse_concrete_field_gsig(
            "Lkotlin/jvm/functions/Function1<\
                 Ljava/util/List<TT;>;\
                 Ljava/util/Set<Ljava/lang/Long;>;>;",
            "Lkotlin/jvm/functions/Function1;",
        ),
        None
    );
}

#[test]
fn field_generic_signature_requires_unparameterized_inner_class_owners() {
    assert_eq!(
        parse_concrete_field_gsig(
            "Lfixture/Outer<Ljava/lang/Integer;>.\
                 Inner<Ljava/lang/Long;>.\
                 Leaf<Ljava/lang/Double;>;",
            "Lfixture/Outer$Inner$Leaf;",
        ),
        None
    );
    assert_eq!(
        parse_concrete_field_gsig(
            "Lfixture/Outer.Inner<Ljava/lang/Long;>.Leaf<Ljava/lang/Double;>;",
            "Lfixture/Outer$Inner$Leaf;",
        ),
        None
    );
    assert_eq!(
        parse_concrete_field_gsig(
            "Lfixture/Outer.Inner.Leaf<Ljava/lang/Double;>;",
            "Lfixture/Outer$Inner$Leaf;",
        ),
        Some(Ty::obj_args("fixture/Outer$Inner$Leaf", &[Ty::Double]))
    );
}

#[test]
fn field_function_shape_comes_from_the_classifier_declaration() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )));
    let valid = parse_concrete_field_gsig(
        "Lkotlin/jvm/functions/Function2<\
                 Ljava/lang/Integer;\
                 Ljava/lang/Long;\
                 Ljava/lang/String;>;",
        "Lkotlin/jvm/functions/Function2;",
    )
    .expect("valid generic field signature");
    assert_eq!(
        libraries.semanticize_jvm_type(valid),
        Ty::fun(vec![Ty::Int, Ty::Long], Ty::String),
    );

    for signature in [
        "Lkotlin/jvm/functions/Function1<Ljava/lang/String;>;",
        "Lkotlin/jvm/functions/Function1<\
             Ljava/lang/Integer;Ljava/lang/Long;Ljava/lang/String;>;",
        "Lkotlin/jvm/functions/FunctionX<Ljava/lang/String;>;",
        "Lkotlin/jvm/functions/Function1Extra<Ljava/lang/String;>;",
        "Lkotlin/jvm/functions/Function+1<Ljava/lang/String;Ljava/lang/String;>;",
        "Lkotlin/jvm/functions/Function<Ljava/lang/String;>;",
    ] {
        let descriptor = format!("L{};", &signature[1..signature.find('<').unwrap()]);
        let raw = parse_concrete_field_gsig(signature, &descriptor)
            .expect("well-formed nominal generic signature");
        assert!(
            !libraries.semanticize_jvm_type(raw).is_fun(),
            "invented a function shape for {signature}"
        );
    }
}

#[test]
fn field_generic_signature_requires_reference_and_matching_erasure() {
    assert_eq!(parse_concrete_field_gsig("I", "I"), None);
    assert_eq!(parse_concrete_field_gsig("V", "V"), None);
    assert_eq!(parse_concrete_field_gsig("[V", "[V"), None);
    assert_eq!(
        parse_concrete_field_gsig("[I", "[I"),
        Some(Ty::array(Ty::Int))
    );
    assert_eq!(
        parse_concrete_field_gsig("Ljava/util/List<Ljava/lang/Integer;>;", "Ljava/util/Set;",),
        None
    );
    assert_eq!(
        parse_concrete_field_gsig("Ljava/util/List<TT;>;", "Ljava/util/List;"),
        None
    );
}
