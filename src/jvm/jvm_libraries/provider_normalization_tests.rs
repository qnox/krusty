use super::JvmLibraries;
use crate::symbol_source::SymbolNamespace;
use crate::types::{type_name, Ty};

fn initialized_libraries(classpath: std::rc::Rc<super::Classpath>) -> JvmLibraries {
    JvmLibraries::new(classpath).expect("JVM provider initialization")
}

#[test]
fn list_storage_plans_use_the_selected_stdlib_and_its_array_list_alias() {
    use crate::libraries::{CompilerIntrinsic, ListStorageConstruction};
    let stdlib = crate::toolchain::stdlib_jar().expect("selected reference stdlib");
    let libraries = initialized_libraries(std::rc::Rc::new(super::Classpath::new(vec![stdlib])));
    for (name, expected) in [
        ("emptyList", &[ListStorageConstruction::ReadonlyEmpty][..]),
        (
            "listOf",
            &[
                ListStorageConstruction::ReadonlyEmpty,
                ListStorageConstruction::ReadonlySingleton,
                ListStorageConstruction::ReadonlyPacked,
            ][..],
        ),
        (
            "mutableListOf",
            &[
                ListStorageConstruction::MutableEmpty,
                ListStorageConstruction::MutablePacked,
            ][..],
        ),
        (
            "arrayListOf",
            &[
                ListStorageConstruction::MutableEmpty,
                ListStorageConstruction::MutablePacked,
            ][..],
        ),
    ] {
        let symbols = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlin/collections")),
            name,
        );
        let functions = symbols.callables.functions();
        assert_eq!(functions.len(), expected.len(), "{name} overloads");
        for &plan in expected {
            assert_eq!(
                functions
                    .iter()
                    .filter(|f| f.callable.compiler_intrinsic
                        == Some(CompilerIntrinsic::ListStorageConstruction(plan)))
                    .count(),
                1,
                "{name} {plan:?}: {:?}",
                functions
                    .iter()
                    .map(|f| (
                        &f.generic_sig,
                        &f.call_sig,
                        f.flags,
                        f.kind,
                        f.receiver,
                        f.callable.compiler_intrinsic
                    ))
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn a_dependency_closed_range_lookalike_keeps_its_body() {
    let directory = std::env::temp_dir().join(format!(
        "krusty-provider-range-origin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let internal = "kotlin/ranges/DependencyRangesKt";
    let descriptor = "(Lkotlin/ranges/IntRange;C)Z";
    let function = crate::metadata::builder::FnMeta {
        spellings: crate::spelling::DeclaredSpellings::default(),
        annotations: Default::default(),
        decl_order: 0,
        jvm_name: None,
        name: "contains".to_string(),
        params: vec![("value".to_string(), Ty::Char)],
        ret: Ty::Boolean,
        receiver: Some(Ty::obj("kotlin/ranges/IntRange")),
        param_modifiers: Vec::new(),
        suspend: false,
        jvm_desc: Some(descriptor.to_string()),
        contract: None,
        inline: false,
        has_function_typed_parameter: false,
        operator: true,
        infix: false,
        tailrec: false,
        companion: false,
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        context_count: 0,
        context_parameter_kinds: Vec::new(),
        vararg_index: None,
        visibility: crate::types::Visibility::Public,
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
    };
    let (d1, d2) =
        crate::metadata::builder::build_package(&[function], &[], &[], (None, &[]), true);
    let mut writer = crate::jvm::classfile::ClassWriter::new(internal, "java/lang/Object");
    let mut body = crate::jvm::classfile::CodeBuilder::new(2);
    body.push_int(0, &mut writer);
    body.ireturn();
    writer.add_method(
        crate::jvm::classfile::ACC_PUBLIC
            | crate::jvm::classfile::ACC_STATIC
            | crate::jvm::classfile::ACC_FINAL,
        "contains",
        descriptor,
        &body,
    );
    writer.set_kotlin_metadata(
        2,
        crate::jvm::classfile::MetadataStamp {
            version: [2, 2, 0],
            pre_release: false,
        },
        48,
        &crate::metadata::encoding::bytes_to_strings(&d1),
        &d2,
    );
    let path = directory.join(format!("{internal}.class"));
    std::fs::create_dir_all(path.parent().expect("package")).expect("create package");
    std::fs::write(&path, writer.finish()).expect("write dependency facade");

    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![directory.clone()],
    )));
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/ranges")),
        "contains",
    );
    let contains = symbols
        .callables
        .functions()
        .iter()
        .find(|function| {
            function.receiver == Some(Ty::obj("kotlin/ranges/IntRange"))
                && function.callable.params == [Ty::obj("kotlin/ranges/IntRange"), Ty::Char]
        })
        .expect("dependency extension");
    assert_eq!(contains.callable.compiler_intrinsic, None);

    drop(symbols);
    drop(libraries);
    std::fs::remove_dir_all(directory).expect("remove dependency facade");
}

#[test]
fn a_foreign_alias_cannot_change_the_stdlib_list_storage_plan() {
    use crate::libraries::{CompilerIntrinsic, ListStorageConstruction};
    use crate::metadata::builder::{TypeAliasFormalMeta, TypeAliasMeta};
    let directory = std::env::temp_dir().join(format!(
        "krusty-storage-alias-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let foreign = type_name("fixture/ForeignList");
    let alias = TypeAliasMeta {
        name: "ArrayList".into(),
        formals: vec![TypeAliasFormalMeta {
            source_name: "E".into(),
            semantic_name: "E".into(),
        }],
        expansion: Ty::obj_args_name(
            foreign,
            &[Ty::ty_param("E", Ty::nullable(Ty::obj("kotlin/Any")))],
        ),
        visibility: crate::types::Visibility::Public,
        expansion_spelling: Default::default(),
        decl_order: 0,
    };
    let (d1, d2) = crate::metadata::builder::build_package(&[], &[], &[alias], (None, &[]), true);
    let internal = "kotlin/collections/ForeignAliasesKt";
    let mut writer = crate::jvm::classfile::ClassWriter::new(internal, "java/lang/Object");
    writer.set_kotlin_metadata(
        2,
        crate::jvm::classfile::MetadataStamp {
            version: [2, 2, 0],
            pre_release: false,
        },
        48,
        &crate::metadata::encoding::bytes_to_strings(&d1),
        &d2,
    );
    let path = directory.join(format!("{internal}.class"));
    std::fs::create_dir_all(path.parent().expect("package")).expect("create foreign alias package");
    std::fs::write(&path, writer.finish()).expect("write foreign alias");
    let identity = type_name("kotlin/collections/ArrayList");
    let foreign_only = super::Classpath::new(vec![directory.clone()]);
    assert!(foreign_only
        .paired_stdlib_type_alias_expansion(identity)
        .is_none());
    let stdlib = crate::toolchain::stdlib_jar().expect("selected reference stdlib");
    let classpath = std::rc::Rc::new(super::Classpath::new(vec![directory.clone(), stdlib]));
    assert_eq!(
        classpath
            .type_alias_expansion(identity)
            .expect("ordinary alias lookup")
            .0,
        foreign
    );
    let libraries = initialized_libraries(classpath);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/collections")),
        "arrayListOf",
    );
    let functions = symbols.callables.functions();
    assert_eq!(functions.len(), 2);
    for plan in [
        ListStorageConstruction::MutableEmpty,
        ListStorageConstruction::MutablePacked,
    ] {
        assert_eq!(
            functions
                .iter()
                .filter(|f| f.callable.compiler_intrinsic
                    == Some(CompilerIntrinsic::ListStorageConstruction(plan)))
                .count(),
            1,
            "{plan:?}"
        );
    }
    drop(symbols);
    drop(libraries);
    drop(foreign_only);
    std::fs::remove_dir_all(directory).expect("remove owned foreign alias fixture");
}

fn required_closed_range_contains_roles(
    libraries: &JvmLibraries,
) -> Vec<Option<crate::libraries::CompilerIntrinsic>> {
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/ranges")),
        "contains",
    );
    let functions = symbols.callables.functions();
    [
        (Ty::obj("kotlin/ranges/IntRange"), Ty::Long),
        (Ty::obj("kotlin/ranges/LongRange"), Ty::Int),
        (
            Ty::obj_args("kotlin/ranges/ClosedRange", &[Ty::Float]),
            Ty::Double,
        ),
        (
            Ty::obj_args("kotlin/ranges/ClosedRange", &[Ty::Double]),
            Ty::Float,
        ),
    ]
    .into_iter()
    .map(|(receiver, value)| {
        functions
            .iter()
            .find(|function| {
                function.generic_sig.as_ref().is_some_and(|signature| {
                    signature.receiver == Some(receiver) && signature.params == [value]
                })
            })
            .unwrap_or_else(|| panic!("missing {receiver:?}.contains({value:?})"))
            .callable
            .compiler_intrinsic
    })
    .collect()
}

/// The role of the stdlib's one generic `rangeTo`, `<T : Comparable<T>> T.rangeTo(that: T)`.
fn comparable_range_to_role(
    libraries: &JvmLibraries,
) -> Option<crate::libraries::CompilerIntrinsic> {
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/ranges")),
        "rangeTo",
    );
    let generic = symbols
        .callables
        .functions()
        .iter()
        .filter(|function| {
            function
                .generic_sig
                .as_ref()
                .is_some_and(|signature| signature.formals.len() == 1)
        })
        .collect::<Vec<_>>();
    let [range_to] = generic[..] else {
        panic!("expected one generic rangeTo, found {}", generic.len());
    };
    range_to.callable.compiler_intrinsic
}

/// The role of the builtin `String?.plus(Any?)` extension.
fn nullable_string_plus_role(
    libraries: &JvmLibraries,
) -> Option<crate::libraries::CompilerIntrinsic> {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "plus");
    let functions = symbols.callables.functions();
    let [plus] = functions
        .iter()
        .filter(|function| function.receiver == Some(Ty::nullable(Ty::String)))
        .collect::<Vec<_>>()[..]
    else {
        panic!("expected one String?.plus");
    };
    plus.callable.compiler_intrinsic
}

/// A build tool's stdlib has its repository file name and no Kotlin distribution beside it. Its
/// builtins are still the compiler's: `kotlin.plus` has no JVM class to call.
#[test]
fn a_repository_named_stdlib_owns_its_builtins() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let directory = std::env::temp_dir().join(format!(
        "krusty-provider-repository-stdlib-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).expect("create repository directory");
    let repository_stdlib = directory.join("kotlin-stdlib-2.3.20.jar");
    std::fs::copy(&stdlib, &repository_stdlib).expect("copy stdlib under its repository name");
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![repository_stdlib],
    )));
    assert_eq!(
        nullable_string_plus_role(&libraries),
        Some(crate::libraries::CompilerIntrinsic::StringPlus)
    );
    drop(libraries);
    std::fs::remove_dir_all(directory).expect("remove repository stdlib");
}

#[test]
fn selected_stdlib_builtins_publish_range_contains_but_an_earlier_copy_does_not() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let trusted = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib.clone()],
    )));
    assert_eq!(
        required_closed_range_contains_roles(&trusted),
        vec![Some(crate::libraries::CompilerIntrinsic::ClosedRangeContains); 4],
        "the exact mixed numeric stdlib overloads carry the intrinsic role"
    );
    assert_eq!(
        comparable_range_to_role(&trusted),
        Some(crate::libraries::CompilerIntrinsic::ComparableRangeTo),
        "the selected stdlib's Comparable rangeTo carries the intrinsic role"
    );
    assert_eq!(
        nullable_string_plus_role(&trusted),
        Some(crate::libraries::CompilerIntrinsic::StringPlus)
    );

    let directory = std::env::temp_dir().join(format!(
        "krusty-provider-builtins-origin-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).expect("create dependency directory");
    let dependency = directory.join("dependency.jar");
    std::fs::copy(&stdlib, &dependency).expect("copy stdlib as an ordinary dependency");

    // Pair the real common declarations with an otherwise empty selected stdlib. The earlier exact
    // stdlib copy supplies both physical facades and .kotlin_builtins declarations, but its ordinary
    // dependency name must keep every one of those declarations untrusted.
    let paired = directory.join("paired");
    std::fs::create_dir_all(&paired).expect("create paired stdlib directory");
    let selected_stdlib = paired.join("kotlin-stdlib.jar");
    zip::ZipWriter::new(std::fs::File::create(&selected_stdlib).expect("create empty stdlib"))
        .finish()
        .expect("finish empty stdlib");
    let common_name = "kotlin-stdlib-wasm-js.klib";
    let common = stdlib.parent().expect("stdlib directory").join(common_name);
    let paired_common = paired.join(common_name);
    std::fs::hard_link(&common, &paired_common)
        .or_else(|_| std::fs::copy(&common, &paired_common).map(|_| ()))
        .expect("pair the selected stdlib with common declarations");

    let shadowed = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![dependency, selected_stdlib],
    )));
    assert_eq!(
        required_closed_range_contains_roles(&shadowed),
        vec![None; 4],
        "an earlier full stdlib copy has identical declarations but no selected-stdlib authority"
    );
    assert_eq!(
        comparable_range_to_role(&shadowed),
        None,
        "an earlier stdlib copy's identical Comparable rangeTo has no selected-stdlib authority"
    );
    assert_eq!(
        nullable_string_plus_role(&shadowed),
        None,
        "an earlier stdlib copy's builtins have no selected-stdlib authority"
    );

    drop(shadowed);
    std::fs::remove_dir_all(directory).expect("remove copied builtins");
}

#[test]
fn paired_stdlib_enum_entries_keeps_its_exact_common_role_through_symbols() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let libraries = initialized_libraries(std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        vec![stdlib],
    )));
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/enums")),
        "enumEntries",
    );
    let exact = symbols
        .callables
        .functions()
        .iter()
        .find(|function| {
            function.flags.reified
                && function.flags.inline == crate::libraries::InlineKind::MustInline
                && function
                    .generic_sig
                    .as_ref()
                    .is_some_and(|signature| signature.params.is_empty())
        })
        .expect("zero-argument inline enumEntries");
    assert_eq!(
        exact.callable.compiler_intrinsic,
        Some(crate::libraries::CompilerIntrinsic::EnumEntries)
    );
}

/// Classpath records are shared across worker compilations, but `@SinceKotlin` filtering belongs to
/// one provider's API version. An older compile must not leave its narrower normalized view behind.
#[test]
fn an_older_api_compile_does_not_narrow_a_later_one_on_the_same_classpath() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    fn set_value_receivers(libraries: &JvmLibraries) -> Vec<Ty> {
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "setValue");
        let mut receivers: Vec<Ty> = symbols
            .callables
            .functions()
            .iter()
            .filter_map(|function| function.receiver)
            .collect();
        receivers.sort_by_key(|receiver| format!("{receiver:?}"));
        receivers
    }
    let shared = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![stdlib.clone()]));
    let older = set_value_receivers(
        &initialized_libraries(shared.clone())
            .with_api_version(crate::language_version::LanguageVersion::new(1, 3)),
    );
    let current_after_older = set_value_receivers(&initialized_libraries(shared));
    let current_alone = set_value_receivers(&initialized_libraries(std::rc::Rc::new(
        crate::jvm::classpath::Classpath::new(vec![stdlib]),
    )));

    assert!(
        older.len() < current_alone.len(),
        "API 1.3 withholds the @SinceKotlin(\"1.4\") property-reference setValue: {older:?}"
    );
    assert_eq!(current_after_older, current_alone);
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
fn mapped_any_equals_publishes_its_semantic_role() {
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
        Ty::obj_name(crate::types::wk::any()),
        "equals",
    );
    let overloads = match selected {
        crate::libraries::Callables::Functions(functions)
        | crate::libraries::Callables::Both { functions, .. } => functions.overloads,
        crate::libraries::Callables::None | crate::libraries::Callables::Properties(_) => {
            Vec::new()
        }
    };
    assert_eq!(overloads.len(), 1);
    let equals = &overloads[0];
    assert_eq!(equals.callable.owner, type_name("java/lang/Object"));
    assert_eq!(
        equals.callable.declaration_owner,
        Some(crate::types::SemanticCallableOwner::Classifier(
            crate::types::wk::any()
        ))
    );
    assert_eq!(
        equals.semantic_params().as_ref(),
        &[Ty::nullable(Ty::obj_name(crate::types::wk::any()))]
    );
    assert_eq!(equals.callable.ret, Ty::Boolean);
    assert!(equals.callable.external_identity.is_some());
    assert_eq!(
        equals.callable.semantic_role,
        Some(crate::types::SemanticCallRole::KotlinAnyEquals)
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

    let callable_declarations = libraries.declared_callables_for(
        Ty::obj("kotlin/reflect/KCallable"),
        "name",
        &[],
        &[],
        &super::AccessorInventory::default(),
    );
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
