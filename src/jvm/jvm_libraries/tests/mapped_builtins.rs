use super::*;

#[test]
fn mapped_number_members_publish_only_their_kotlin_names() {
    let (Some(stdlib), Some(jdk)) = (
        crate::toolchain::stdlib_jar(),
        crate::toolchain::jdk_modules(),
    ) else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib, jdk],
    )));
    let classifier = libraries
        .classifier_record(type_name("java/lang/Number"))
        .expect("Number classifier");
    assert!(classifier.members.iter().any(
        |member| member.name == "toInt" && member.physical_name.as_deref() == Some("intValue")
    ));
    assert!(
        classifier
            .members
            .iter()
            .all(|member| member.name != "intValue"),
        "provider boundary must not leak the physical Java spelling"
    );
}

#[test]
fn mapped_kotlin_number_publishes_jdk_realizations_without_stdlib() {
    let Some(jdk) = crate::toolchain::jdk_modules() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![jdk],
    )));
    let classifier = libraries
        .classifier_record(type_name("kotlin/Number"))
        .expect("kotlin.Number");
    for (source, physical) in [
        ("toByte", "byteValue"),
        ("toShort", "shortValue"),
        ("toInt", "intValue"),
        ("toLong", "longValue"),
        ("toFloat", "floatValue"),
        ("toDouble", "doubleValue"),
    ] {
        assert!(
            classifier.members.iter().any(|member| {
                member.name == source && member.physical_name.as_deref() == Some(physical)
            }),
            "missing {source} realized by {physical}"
        );
    }
    assert!(classifier.members.iter().all(|member| {
        !matches!(
            member.name.as_str(),
            "byteValue" | "shortValue" | "intValue" | "longValue" | "floatValue" | "doubleValue"
        )
    }));
}

#[test]
fn mapped_throwable_constructors_keep_kotlin_parameter_names_and_jvm_realizations() {
    let (Some(stdlib), Some(jdk)) = (
        crate::toolchain::stdlib_jar(),
        crate::toolchain::jdk_modules(),
    ) else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib, jdk],
    )));
    let classifier = libraries
        .classifier_record(type_name("kotlin/Throwable"))
        .expect("Throwable classifier");
    let constructor = classifier
        .constructors
        .iter()
        .find(|constructor| constructor.call_sig.param_names.as_slice() == ["message", "cause"])
        .expect("Kotlin Throwable(message, cause) declaration");

    assert_eq!(
        constructor.params,
        [
            Ty::nullable(Ty::String),
            Ty::nullable(Ty::obj("kotlin/Throwable"))
        ]
    );
    assert_eq!(
        constructor.physical_params,
        [
            Ty::platform_nullable(Ty::String),
            Ty::platform_nullable(Ty::obj("kotlin/Throwable"))
        ]
    );
    assert_eq!(
        constructor.descriptor,
        "(Ljava/lang/String;Ljava/lang/Throwable;)V"
    );
}

#[test]
fn mapped_mutable_list_publishes_its_applied_read_only_supertype() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )));
    let actual = Ty::obj_args("kotlin/collections/MutableList", &[Ty::String]);
    let supertypes = crate::symbol_resolver::direct_supertypes(&libraries, actual);
    assert!(
        supertypes.contains(&Ty::obj_args("kotlin/collections/List", &[Ty::String])),
        "MutableList<String> direct supertypes: {supertypes:?}"
    );
    assert!(crate::symbol_resolver::resolution_subtype(
        &libraries,
        actual,
        Ty::obj_args("kotlin/collections/List", &[Ty::String]),
    ));
}
