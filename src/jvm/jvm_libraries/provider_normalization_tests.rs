use super::JvmLibraries;
use crate::types::{type_name, Ty};

fn initialized_libraries(classpath: std::rc::Rc<super::Classpath>) -> JvmLibraries {
    JvmLibraries::new(classpath).expect("JVM provider initialization")
}

#[test]
fn physical_object_superclass_is_semantic_any_before_receiver_selection() {
    let directory = std::env::temp_dir().join(format!(
        "krusty-provider-root-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let internal = "probe/provider6044/Leaf";
    let path = directory.join(format!("{internal}.class"));
    std::fs::create_dir_all(path.parent().expect("package")).expect("create package");
    let bytes = crate::jvm::classfile::ClassWriter::new(internal, "java/lang/Object").finish();
    std::fs::write(&path, bytes).expect("write class");

    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![directory.clone()],
    )));
    let leaf = type_name(internal);
    let any = crate::types::wk::any();
    let classifier = libraries
        .classifier_record(leaf)
        .expect("synthetic Java classifier");
    assert_eq!(
        classifier.supertypes.iter_ids().collect::<Vec<_>>(),
        vec![any]
    );
    assert_eq!(classifier.supertype_templates, vec![Ty::obj_name(any)]);
    assert_eq!(
        crate::symbol_resolver::ReceiverMro::new(&libraries, Ty::obj_name(leaf))
            .rank(&libraries, Ty::obj_name(any)),
        Some(1),
        "semantic Any must be a declared MRO rung, not the universal fallback"
    );

    drop(libraries);
    std::fs::remove_dir_all(directory).expect("remove provider-root directory");
}

fn zero_arg_clone_owners(
    libraries: &JvmLibraries,
    owner: crate::types::TypeName,
) -> Vec<crate::types::TypeName> {
    let selected =
        crate::symbol_resolver::members_in_hierarchy(libraries, Ty::obj_name(owner), "clone");
    let overloads = match selected {
        crate::libraries::Callables::Functions(functions)
        | crate::libraries::Callables::Both { functions, .. } => functions.overloads,
        crate::libraries::Callables::None | crate::libraries::Callables::Properties(_) => {
            Vec::new()
        }
    };
    overloads
        .into_iter()
        .filter(|function| function.callable.params.is_empty())
        .map(|function| {
            assert_eq!(function.visibility, crate::types::Visibility::Protected);
            assert!(!function.flags.is_final);
            assert!(!function.flags.is_abstract);
            function.callable.owner
        })
        .collect()
}

#[test]
fn erased_top_clone_realizes_object_through_semantic_any() {
    let (Some(stdlib), Some(jdk)) = (
        crate::toolchain::stdlib_jar(),
        crate::toolchain::jdk_modules(),
    ) else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib, jdk],
    )));
    let object_owner = crate::types::wk::java_object();
    let any = crate::types::wk::any();
    let object_classifier = libraries
        .classifier_record(object_owner)
        .expect("java.lang.Object");
    assert!(
        object_classifier
            .supertypes
            .iter_ids()
            .any(|supertype| crate::types::same(supertype, any)),
        "Object's semantic superclass must stay kotlin/Any"
    );
    assert!(
        !object_classifier
            .supertypes
            .iter_ids()
            .any(|supertype| crate::types::same(supertype, object_owner)),
        "Object must not publish itself as a semantic supertype"
    );
    for classifier in [any, object_owner, type_name("kotlin/Cloneable")] {
        assert_eq!(
            zero_arg_clone_owners(&libraries, classifier),
            vec![object_owner],
            "protected clone on {}",
            classifier.render()
        );
    }
}

#[test]
fn concrete_java_collection_keeps_its_kotlin_interface_faces() {
    let (Some(stdlib), Some(jdk)) = (
        crate::toolchain::stdlib_jar(),
        crate::toolchain::jdk_modules(),
    ) else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib, jdk],
    )));
    let list = libraries
        .classifier_record(type_name("kotlin/collections/List"))
        .expect("Kotlin List classifier");
    assert_eq!(
        list.mapped_collection,
        Some(crate::types::MappedCollection {
            kind: crate::types::CollectionKind::List,
            mutable: false,
        }),
        "the semantic collection face must cross the provider boundary"
    );
    let classifier = libraries
        .classifier_record(type_name("java/util/ArrayList"))
        .expect("ArrayList classifier");
    assert!(
        classifier
            .supertypes
            .contains("kotlin/collections/MutableList"),
        "ArrayList supertypes: {:?}",
        classifier.supertypes
    );
}

#[test]
fn comparable_compare_to_publishes_its_semantic_role() {
    let (Some(stdlib), Some(jdk)) = (
        crate::toolchain::stdlib_jar(),
        crate::toolchain::jdk_modules(),
    ) else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib, jdk],
    )));
    let selected = crate::symbol_resolver::members_in_hierarchy(
        &libraries,
        Ty::obj_name(crate::types::wk::comparable()),
        "compareTo",
    );
    let overloads = match selected {
        crate::libraries::Callables::Functions(functions)
        | crate::libraries::Callables::Both { functions, .. } => functions.overloads,
        crate::libraries::Callables::None | crate::libraries::Callables::Properties(_) => {
            Vec::new()
        }
    };
    assert_eq!(overloads.len(), 1);
    assert_eq!(
        overloads[0].callable.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinComparableCompareTo)
    );
}

#[test]
fn reflection_declarations_publish_their_semantic_roles() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )));

    let callable_declarations =
        libraries.declared_callables_for(Ty::obj("kotlin/reflect/KCallable"), "name", &[], &[]);
    let callable = callable_declarations
        .properties()
        .first()
        .expect("KCallable.name");
    assert_eq!(
        callable.getter.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinCallableReferenceName)
    );

    let property = crate::symbol_resolver::members_in_hierarchy(
        &libraries,
        Ty::obj_args("kotlin/reflect/KProperty1", &[Ty::String, Ty::Int]),
        "get",
    );
    let get = property.functions().first().expect("KProperty1.get(T)");
    assert_eq!(
        get.callable.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinPropertyReferenceGet(
            1
        ))
    );

    let mutable = crate::symbol_resolver::members_in_hierarchy(
        &libraries,
        Ty::obj_args("kotlin/reflect/KMutableProperty1", &[Ty::String, Ty::Int]),
        "set",
    );
    let set = mutable
        .functions()
        .first()
        .expect("KMutableProperty1.set(T, V)");
    assert_eq!(
        set.callable.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinPropertyReferenceSet(
            1
        ))
    );
}
