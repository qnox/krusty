//! A body's read of a dependency enum constant materializes the enum class in its unit, which the
//! shared enum realization then builds as it builds a source enum.

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
            &KlibCalleeFacts::default().with_type_oracle(&oracle),
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
    crate::backend::enums::realize(&mut realized, crate::backend::enums::Contract::KotlinNative);
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
    let (class, name) =
        super::super::enum_classes::authenticate_entry(bodies, module, &entry("RESUMED"))
            .unwrap_or_else(|decline| panic!("{decline:?}"));
    assert_eq!(name, "RESUMED");
    assert_eq!(class.classifier, type_name(COROUTINE_SINGLETONS));
    assert_eq!(class.constructor.module, *module);
    assert_eq!(
        class.entries,
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
fn an_enum_with_members_beyond_its_generated_ones_declines() {
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
    // `DeprecationLevel` declares only its constants. `RegexOption`'s constants take constructor
    // arguments, which it stores in member properties.
    let level = KlibPublicIdSignature::classifier(type_name("kotlin/DeprecationLevel.ERROR"));
    assert!(super::super::enum_classes::authenticate_entry(bodies, module, &level).is_ok());
    let option =
        KlibPublicIdSignature::classifier(type_name("kotlin/text/RegexOption.IGNORE_CASE"));
    assert_eq!(
        super::super::enum_classes::authenticate_entry(bodies, module, &option).err(),
        Some(KlibBodyDeclineReason::UnsupportedDeclaration(
            "an enum class with member properties"
        ))
    );
}
