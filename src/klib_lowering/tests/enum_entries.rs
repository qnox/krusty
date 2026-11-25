//! A body's read of a dependency enum constant materializes the enum class in its unit from its
//! admitted initialization, which the shared enum realization then builds as it builds a source
//! enum.

use super::*;

const COROUTINE_SINGLETONS: &str = "kotlin/coroutines/intrinsics/CoroutineSingletons";

/// `COROUTINE_SUSPENDED`'s getter, whose body reads `CoroutineSingletons.COROUTINE_SUSPENDED`.
fn coroutine_suspended_getter(
    libraries: &KlibLibraries,
) -> crate::libraries::ExternalCallableRealization {
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("kotlin/coroutines/intrinsics")),
        "COROUTINE_SUSPENDED",
    );
    let [property] = symbols.callables.properties() else {
        panic!("one COROUTINE_SUSPENDED property");
    };
    libraries
        .external_callable(
            property
                .getter
                .external_identity
                .expect("provider identity"),
        )
        .expect("exact declaration")
}

/// The facts of a closure that read an instance of each of `classes`, for a target that lays out
/// `kotlin.Enum` and `kotlin.Any`.
fn reading<'a>(
    libraries: &KlibLibraries,
    classes: &[&str],
    oracle: &'a crate::symbol_resolver::SourceOracle<'a>,
) -> KlibCalleeFacts<'a> {
    fn represents(classifier: TypeName) -> bool {
        classifier == crate::types::wk::kotlin_enum() || classifier == crate::types::wk::any()
    }
    KlibCalleeFacts::default()
        .with_type_oracle(oracle)
        .with_target_classifiers(&represents)
        .with_read_classes(
            classes
                .iter()
                .map(|class| {
                    libraries
                        .klib_body_class_contract(type_name(class))
                        .expect("a serialized class contract")
                })
                .collect(),
        )
}

fn entry(name: &str) -> KlibPublicIdSignature {
    KlibPublicIdSignature::classifier(type_name(&format!("{COROUTINE_SINGLETONS}.{name}")))
}

#[test]
fn a_dependency_enum_constant_read_materializes_its_enum_class() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let getter = coroutine_suspended_getter(&libraries);
    let mut unit = DependencyBodyUnit::default();
    // The getter answers its constant as `Any`, a widening the calling compilation's class model
    // proves.
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    let function = unit
        .lower_function(
            getter.klib_body_callable().expect("signed"),
            libraries.declaration_bodies(),
            &reading(&libraries, &[COROUTINE_SINGLETONS], &oracle),
        )
        .unwrap_or_else(|decline| panic!("COROUTINE_SUSPENDED getter: {decline}"));
    let classifier = type_name(COROUTINE_SINGLETONS);
    let ir = unit.ir();
    let class = &ir.classes[ir.class_id_by_name(classifier).expect("materialized") as usize];
    assert!(class.is_enum);
    assert_eq!(class.superclass, crate::types::wk::kotlin_enum());
    assert_eq!(
        class.supertypes,
        [Ty::obj_args_name(
            crate::types::wk::kotlin_enum(),
            &[Ty::obj_name(classifier)]
        )]
    );
    assert_eq!(
        class
            .enum_entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["COROUTINE_SUSPENDED", "UNDECIDED", "RESUMED"]
    );
    assert_eq!(
        unit.materialized_classes()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        [classifier]
    );
    assert_eq!(unit.allocating_constructors().count(), 0);
    let reads = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::EnumEntry { classifier, name } => Some((*classifier, &**name)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reads, [(classifier, "COROUTINE_SUSPENDED")]);
    assert!(ir.functions[function as usize].body.is_some());

    // The shared realization builds the constants and leaves no use node behind.
    let mut realized = unit.into_ir();
    crate::backend::enums::realize(
        &mut realized,
        crate::backend::enums::Contract::KotlinNative,
        None,
        &mut crate::backend::DependencyExportSignatures::default(),
    )
    .expect("the Native contract builds its own list");
    assert!(!realized
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::EnumEntry { .. })));
}

#[test]
fn an_enum_constant_resolves_in_the_reading_body_s_producer_graph() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let getter = coroutine_suspended_getter(&libraries);
    let module = &getter
        .klib_body_callable()
        .expect("signed")
        .identity()
        .module;
    let (classifier, name) =
        super::super::enum_classes::authenticate_entry(bodies, module, &entry("RESUMED"))
            .unwrap_or_else(|decline| panic!("{decline:?}"));
    assert_eq!(name, "RESUMED");
    assert_eq!(classifier, type_name(COROUTINE_SINGLETONS));

    // The admitted initialization of the enum is the one the producer graph declares: its
    // constructor's module, and its constants in declaration order.
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    let facts = reading(&libraries, &[COROUTINE_SINGLETONS], &oracle);
    let class = facts
        .read_class(type_name(COROUTINE_SINGLETONS))
        .expect("the closure read the enum");
    let plan = super::super::class_initialization::declared_instances_plan(class, bodies, &facts)
        .unwrap_or_else(|decline| panic!("{decline:?}"))
        .expect("an enum declaring only its constants is admitted");
    assert_eq!(plan.header.contract.module, *module);
    assert_eq!(
        plan.entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["COROUTINE_SUSPENDED", "UNDECIDED", "RESUMED"]
    );

    // An entry no producer module declares is not an enum constant of the graph.
    assert_eq!(
        super::super::enum_classes::authenticate_entry(bodies, module, &entry("SUSPENDED")).err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(
            "an enum entry has no serialized declaring class".to_owned()
        ))
    );
    // Nor does a module outside the graph lend its declarations to a body.
    assert_eq!(
        super::super::enum_classes::authenticate_entry(bodies, "unrelated", &entry("RESUMED"))
            .err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(
            "an enum entry has no serialized declaring class".to_owned()
        ))
    );
}

#[test]
fn an_enum_with_constructor_state_admits_its_initialization() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    // `CharCategory`'s constants pass its constructor the value and code it stores in member
    // properties; the enum also declares a companion, a member function and the members the
    // compiler gives every enum.
    const CHAR_CATEGORY: &str = "kotlin/text/CharCategory";
    let facts = reading(&libraries, &[CHAR_CATEGORY], &oracle);
    let class = facts
        .read_class(type_name(CHAR_CATEGORY))
        .expect("the closure read the enum");
    let plan = super::super::class_initialization::declared_instances_plan(class, bodies, &facts)
        .unwrap_or_else(|decline| panic!("{decline:?}"))
        .expect("an enum with constructor state is admitted");
    let names = plan
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names.first(), Some(&"UNASSIGNED"));
    assert!(plan
        .entries
        .iter()
        .all(|entry| entry.arguments.len() == plan.header.function.regular_parameters.len()));
    assert!(!plan.header.function.regular_parameters.is_empty());
    let superclass = plan.superclass.as_ref().expect("kotlin.Enum's constructor");
    assert_eq!(superclass.owner, crate::types::wk::kotlin_enum());
    assert!(!superclass.materialized && superclass.arguments.is_empty());
    assert_eq!(
        plan.supertypes.first(),
        Some(&Ty::obj_args_name(
            crate::types::wk::kotlin_enum(),
            &[Ty::obj_name(type_name(CHAR_CATEGORY))]
        ))
    );
}

/// The exact serialized identity of `DeprecationLevel`'s implicit member `name`.
fn deprecation_level_member(libraries: &KlibLibraries, name: &str) -> KlibDependencyCallable {
    let symbols = libraries.symbols(
        SymbolNamespace::Classifier(type_name("kotlin/DeprecationLevel")),
        name,
    );
    let [member] = symbols.callables.functions() else {
        panic!("one {name}");
    };
    libraries
        .external_callable(
            member
                .callable
                .external_identity
                .expect("provider identity"),
        )
        .expect("exact declaration")
        .dependency_callable
        .expect("a serialized declaration")
}

#[test]
fn an_enum_s_generated_members_carry_the_roles_of_the_common_enum_operations() {
    use crate::types::SemanticCallRole;
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    for (name, expected) in [
        ("values", SemanticCallRole::KotlinEnumValues),
        ("valueOf", SemanticCallRole::KotlinEnumValueOf),
    ] {
        let identity = deprecation_level_member(&libraries, name);
        let (_, realization) = libraries
            .external_callable_by_dependency(&identity)
            .unwrap_or_else(|| panic!("{name} has a provider declaration"));
        let selected = realization
            .klib_body_callable()
            .unwrap_or_else(|| panic!("{name} has a serialized declaration"));
        assert_eq!(selected.semantic_role(), Some(expected), "{name}");
        assert_eq!(
            realization.declaration_owner,
            Some(crate::types::SemanticCallableOwner::Classifier(type_name(
                "kotlin/DeprecationLevel"
            ))),
            "{name}"
        );
    }
    // The generated members belong to the enum whose entries they answer for, in order.
    const DEPRECATION_LEVEL: &str = "kotlin/DeprecationLevel";
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    let facts = reading(&libraries, &[DEPRECATION_LEVEL], &oracle);
    let class = facts
        .read_class(type_name(DEPRECATION_LEVEL))
        .expect("the closure read the enum");
    let plan = super::super::class_initialization::declared_instances_plan(
        class,
        libraries.declaration_bodies(),
        &facts,
    )
    .unwrap_or_else(|decline| panic!("{decline:?}"))
    .expect("an enum declaring only its constants is admitted");
    assert_eq!(
        plan.entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["WARNING", "ERROR", "HIDDEN"]
    );
    // Any other callable links as an ordinary call.
    let getter = coroutine_suspended_getter(&libraries);
    assert_eq!(
        getter
            .klib_body_callable()
            .expect("a serialized declaration")
            .semantic_role(),
        None
    );
}

#[test]
fn an_enum_declaring_only_its_constants_admits_its_initialization() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let getter = coroutine_suspended_getter(&libraries);
    let module = &getter
        .klib_body_callable()
        .expect("signed")
        .identity()
        .module;
    // `DeprecationLevel` declares only its constants: its constructor takes no parameter.
    let level = KlibPublicIdSignature::classifier(type_name("kotlin/DeprecationLevel.ERROR"));
    assert!(super::super::enum_classes::authenticate_entry(bodies, module, &level).is_ok());
    const DEPRECATION_LEVEL: &str = "kotlin/DeprecationLevel";
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    let facts = reading(&libraries, &[DEPRECATION_LEVEL], &oracle);
    let class = facts
        .read_class(type_name(DEPRECATION_LEVEL))
        .expect("the closure read the enum");
    let plan = super::super::class_initialization::declared_instances_plan(class, bodies, &facts)
        .unwrap_or_else(|decline| panic!("{decline:?}"))
        .expect("an enum declaring only its constants is admitted");
    assert_eq!(
        plan.entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["WARNING", "ERROR", "HIDDEN"]
    );
    assert!(plan.entries.iter().all(|entry| entry.arguments.is_empty()));
    assert!(plan.header.function.regular_parameters.is_empty());
}

#[test]
fn an_enum_constructor_with_a_defaulted_parameter_keeps_its_target_boundary() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let getter = coroutine_suspended_getter(&libraries);
    let module = &getter
        .klib_body_callable()
        .expect("signed")
        .identity()
        .module;
    // `RegexOption`'s constants take constructor arguments, which it stores in member properties,
    // and its constructor's `mask` parameter defaults to `value`. A default value is lowered in
    // the call frame of a constructor call, not in the entry's initializer frame, so the class is
    // not admitted: its initialization stays the target's, and the plan declines without an
    // initialization rather than lowering a partial one.
    let option =
        KlibPublicIdSignature::classifier(type_name("kotlin/text/RegexOption.IGNORE_CASE"));
    assert!(super::super::enum_classes::authenticate_entry(bodies, module, &option).is_ok());
    const REGEX_OPTION: &str = "kotlin/text/RegexOption";
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    let facts = reading(&libraries, &[REGEX_OPTION], &oracle);
    let class = facts
        .read_class(type_name(REGEX_OPTION))
        .expect("the closure read the enum");
    assert!(
        super::super::class_initialization::declared_instances_plan(class, bodies, &facts)
            .expect("a defaulted parameter is not a mismatch")
            .is_none()
    );
}
