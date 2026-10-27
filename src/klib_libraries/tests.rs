use std::rc::Rc;

mod accessor_modifiers;
mod declaration_names;

use super::*;
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::{ExternalCallableKind, FnKind, FunctionInfo, KlibDeclarationSignature};
use crate::metadata::id_signature::{package_function_signature, MetadataContainer};
use crate::metadata::semantic::{
    KotlinFunction, KotlinFunctionTypeShape, KotlinProperty, KotlinType, KotlinTypeAlias,
    KotlinTypeParameter, KotlinTypeParameterId,
};
use crate::types::{
    type_name, ContextParameterKind, SemanticCallableOwner, Ty, TyKind, TypeVariance, Visibility,
};

fn class(internal: &str, nullable: bool) -> KotlinType {
    KotlinType::Class {
        internal: internal.to_owned(),
        args: Vec::new(),
        nullable,
        shape: KotlinFunctionTypeShape::default(),
    }
}

fn function(name: &str, receiver: Option<KotlinType>, params: Vec<KotlinType>) -> KotlinFunction {
    KotlinFunction {
        name: name.to_owned(),
        receiver,
        param_names: vec!["value".to_owned(); params.len()],
        param_defaults: vec![false; params.len()],
        inline_modifiers: vec![crate::types::InlineParameterModifier::None; params.len()],
        params,
        ret: class("kotlin/Unit", false),
        formals: Vec::new(),
        vararg: None,
        visibility: Visibility::Public,
        modality: crate::metadata::semantic::KotlinModality::Final,
        is_inline: false,
        has_reified_type_params: false,
        is_suspend: false,
        is_operator: false,
        is_infix: false,
        is_synthesized: false,
        is_expect: false,
        is_external: false,
        is_static: false,
        context_count: 0,
        context_kinds: Vec::new(),
        annotations: Vec::new(),
        annotation_applications: Vec::new(),
        return_value_status: Default::default(),
        contract: None,
    }
}

fn segments(package: &[&str]) -> Vec<String> {
    package
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect()
}

fn package_of(functions: Vec<KotlinFunction>) -> KotlinPackage {
    KotlinPackage {
        functions,
        ..KotlinPackage::default()
    }
}

#[test]
fn prepared_inline_selection_takes_every_inline_declaration_of_its_module() {
    let parameter = KotlinTypeParameter {
        id: KotlinTypeParameterId(1),
        name: "T".to_owned(),
        bounds: vec![class("kotlin/Any", true)],
        variance: TypeVariance::Invariant,
        only_input: false,
        reified: true,
    };
    let mut selected = function("selected", None, Vec::new());
    selected.is_inline = true;
    selected.has_reified_type_params = true;
    selected.formals = vec![parameter.clone()];
    let mut ordinary_inline = function("ordinaryInline", None, Vec::new());
    ordinary_inline.is_inline = true;
    let mut non_inline_reified = function("nonInlineReified", None, Vec::new());
    non_inline_reified.has_reified_type_params = true;
    non_inline_reified.formals = vec![parameter];
    let mut direct_lambda = function("directLambda", None, vec![class("kotlin/Function0", false)]);
    direct_lambda.is_inline = true;
    direct_lambda.annotations = vec![type_name("kotlin/internal/InlineOnly")];
    let mut materialized_lambda = function(
        "materializedLambda",
        None,
        vec![class("kotlin/Function0", false)],
    );
    materialized_lambda.is_inline = true;
    materialized_lambda.annotations = vec![type_name("kotlin/internal/InlineOnly")];
    materialized_lambda.inline_modifiers = vec![crate::types::InlineParameterModifier::Noinline];

    let inventory = inventory::PackageInventory::from_package_inputs(vec![(
        segments(&["fixture"]),
        package_of(vec![
            selected,
            ordinary_inline,
            non_inline_reified,
            direct_lambda,
            materialized_lambda,
        ]),
        false,
        false,
        Some("fixture-module".to_owned()),
    )])
    .expect("the fixture functions are signable");
    let signature = |name| {
        crate::metadata::klib_ir::KlibIrSignature::Public(
            inventory
                .functions(type_name("fixture"), name)
                .next()
                .expect("the fixture function is published")
                .signature
                .clone(),
        )
    };
    let selected = signature("selected");
    let ordinary_inline = signature("ordinaryInline");
    let non_inline_reified = signature("nonInlineReified");
    let direct_lambda = signature("directLambda");
    let materialized_lambda = signature("materializedLambda");
    let selection = inventory.inline_body_selection();

    assert!(selection.contains("fixture-module", &selected));
    assert!(!selection.contains("other-module", &selected));
    assert!(selection.contains("fixture-module", &ordinary_inline));
    assert!(!selection.contains("fixture-module", &non_inline_reified));
    assert!(selection.contains("fixture-module", &direct_lambda));
    assert!(selection.contains("fixture-module", &materialized_lambda));
}

#[test]
fn only_an_inline_only_or_reified_inline_function_must_be_inlined() {
    use crate::libraries::InlineKind;

    let mut ordinary = function("ordinary", None, Vec::new());
    ordinary.is_inline = true;
    let mut inline_only = function("inlineOnly", None, Vec::new());
    inline_only.is_inline = true;
    inline_only.annotations = vec![type_name("kotlin/internal/InlineOnly")];
    let mut reified = function("reified", None, Vec::new());
    reified.is_inline = true;
    reified.has_reified_type_params = true;
    reified.formals = vec![KotlinTypeParameter {
        id: KotlinTypeParameterId(1),
        name: "T".to_owned(),
        bounds: vec![class("kotlin/Any", true)],
        variance: TypeVariance::Invariant,
        only_input: false,
        reified: true,
    }];
    let plain = function("plain", None, Vec::new());
    let libraries = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_of(vec![ordinary, inline_only, reified, plain]),
    )])
    .expect("the functions are signable");

    let inline = |name| single_function(&libraries, "p", name).callable.inline;
    assert_eq!(
        [
            inline("ordinary"),
            inline("inlineOnly"),
            inline("reified"),
            inline("plain")
        ],
        [
            InlineKind::CanInline,
            InlineKind::MustInline,
            InlineKind::MustInline,
            InlineKind::None
        ]
    );
}

#[test]
fn a_package_type_alias_is_published_from_its_metadata_declaration() {
    let upper = Ty::nullable(Ty::obj("kotlin/Any"));
    let parameter = Ty::ty_param("T", upper);
    let expansion = Ty::fun(vec![parameter], Ty::String);
    let fragment = crate::metadata::klib_fragment::package_fragment(
        &["fixture"],
        &crate::metadata::klib_fragment::KlibFileMembers {
            file_name: "aliases.kt".to_string(),
            functions: Vec::new(),
            properties: Vec::new(),
            constants: Vec::new(),
            aliases: vec![crate::metadata::builder::TypeAliasMeta {
                name: "Transform".to_string(),
                formals: vec![crate::metadata::builder::TypeAliasFormalMeta {
                    source_name: "T".to_string(),
                    semantic_name: "T".to_string(),
                }],
                expansion,
                visibility: Visibility::Public,
                expansion_spelling: Default::default(),
                decl_order: 0,
            }],
        },
        &[],
        true,
    );
    let package = crate::metadata::semantic::parse_package_fragment_checked(&fragment)
        .expect("the alias fragment decodes");
    let libraries = KlibLibraries::from_packages(vec![(segments(&["fixture"]), package)])
        .expect("the alias package is publishable");

    let identity = type_name("fixture/Transform");
    let target = type_name("kotlin/Function1");
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), "Transform");
    assert_eq!(symbols.classifier_name, Some(target));
    assert!(symbols.classifier.is_some());
    assert_eq!(
        symbols.classifier_declaration,
        Some(crate::libraries::ClassifierDeclaration::TypeAlias(
            crate::libraries::AliasExpansion {
                identity,
                declaration_name: Some(std::sync::Arc::new(
                    crate::types::ClassifierDeclarationName::TopLevel {
                        package: type_name("fixture"),
                        name: "Transform".into(),
                    },
                )),
                target,
                formals: vec!["T".to_string()],
                expansion,
                expansion_spelling: crate::spelling::Spelled {
                    args: vec![Default::default(), Default::default()],
                    ..Default::default()
                },
            }
        ))
    );
    assert_eq!(
        <KlibLibraries as crate::libraries::SemanticPlatform>::type_alias_expansion(
            &libraries, identity,
        ),
        Some(crate::libraries::AliasExpansion {
            identity,
            declaration_name: Some(std::sync::Arc::new(
                crate::types::ClassifierDeclarationName::TopLevel {
                    package: type_name("fixture"),
                    name: "Transform".into(),
                },
            )),
            target,
            formals: vec!["T".to_string()],
            expansion,
            expansion_spelling: crate::spelling::Spelled {
                args: vec![Default::default(), Default::default()],
                ..Default::default()
            },
        })
    );
}

#[test]
fn a_friend_top_level_alias_is_not_hidden_by_an_equal_nonfriend_identity() {
    let alias = |expansion: &str| KotlinTypeAlias {
        name: "Alias".to_owned(),
        formals: Vec::new(),
        expansion: class(expansion, false),
        expansion_spelling: Default::default(),
        visibility: Visibility::Internal,
    };
    let package = segments(&["shared"]);
    let inventory = inventory::PackageInventory::from_package_inputs(vec![
        (
            package.clone(),
            KotlinPackage {
                type_aliases: vec![alias("kotlin/Int")],
                ..KotlinPackage::default()
            },
            false,
            false,
            Some("ordinary".to_owned()),
        ),
        (
            package,
            KotlinPackage {
                type_aliases: vec![alias("kotlin/String")],
                ..KotlinPackage::default()
            },
            false,
            true,
            Some("friend".to_owned()),
        ),
    ])
    .expect("equal aliases from distinct modules remain signable");
    let libraries = KlibLibraries::over(Rc::new(inventory));

    assert_eq!(
        <KlibLibraries as crate::libraries::SemanticPlatform>::type_alias_expansion(
            &libraries,
            type_name("shared/Alias"),
        )
        .map(|alias| alias.expansion),
        Some(Ty::String),
        "the exact friend archive owns the selected alias expansion"
    );
}

fn println() -> KotlinFunction {
    let mut println = function("println", None, vec![class("kotlin/Any", true)]);
    println.param_names = vec!["message".to_owned()];
    println
}

fn property_reference_get_value() -> KotlinFunction {
    let parameter = KotlinTypeParameterId(0);
    let value = KotlinType::Param {
        name: "V".to_owned(),
        id: parameter,
        nullable: false,
    };
    let mut get_value = function(
        "getValue",
        Some(KotlinType::Class {
            internal: "kotlin/reflect/KProperty0".to_owned(),
            args: vec![value.clone()],
            nullable: false,
            shape: KotlinFunctionTypeShape::default(),
        }),
        vec![
            class("kotlin/Any", true),
            KotlinType::Class {
                internal: "kotlin/reflect/KProperty".to_owned(),
                args: vec![KotlinType::Star],
                nullable: false,
                shape: KotlinFunctionTypeShape::default(),
            },
        ],
    );
    get_value.param_names = vec!["thisRef".to_owned(), "property".to_owned()];
    get_value.ret = value;
    get_value.formals = vec![KotlinTypeParameter {
        id: parameter,
        name: "V".to_owned(),
        bounds: vec![class("kotlin/Any", true)],
        variance: TypeVariance::Invariant,
        only_input: false,
        reified: false,
    }];
    get_value.is_operator = true;
    get_value
}

fn kotlin_io() -> KlibLibraries {
    KlibLibraries::from_packages(vec![(
        segments(&["kotlin", "io"]),
        package_of(vec![println()]),
    )])
    .expect("an in-memory kotlin.io package is signable")
}

fn expected_signature(package: &[&str], declaration: &KotlinFunction) -> KlibDeclarationSignature {
    let package = segments(package);
    let container = MetadataContainer {
        package: &package,
        classes: &[],
        native_interop_library: false,
    };
    KlibDeclarationSignature::Public(
        package_function_signature(container, declaration).expect("fixture is signable"),
    )
}

#[test]
fn a_companion_extension_reverse_joins_by_its_exact_module_and_signature() {
    let mut from_bits = function(
        "fromBits",
        Some(class("kotlin/Float", false)),
        vec![class("kotlin/Int", false)],
    );
    from_bits.is_static = true;
    let identity = crate::libraries::KlibDependencyCallable {
        module: "stdlib".to_owned(),
        declaration: expected_signature(&["kotlin"], &from_bits),
    };
    let inventory = inventory::PackageInventory::from_package_inputs(vec![(
        segments(&["kotlin"]),
        package_of(vec![from_bits]),
        true,
        false,
        Some(identity.module.clone()),
    )])
    .expect("the companion extension is signable");

    let (placement, signed) = inventory
        .dependency_function(&identity)
        .expect("the exact companion extension identity reverse-joins");
    assert_eq!(
        placement,
        crate::libraries::CallablePlacement::Associated {
            classifier: type_name("kotlin/Float"),
            declaration_owner: SemanticCallableOwner::Package(type_name("kotlin")),
            access_owner: None,
        }
    );
    assert!(signed.compiler_builtin_owner);
    assert_eq!(signed.module.as_deref(), Some("stdlib"));
}

fn single_function(libraries: &KlibLibraries, package: &str, name: &str) -> FunctionInfo {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
    assert!(symbols.classifier.is_none());
    assert!(symbols.callables.properties().is_empty());
    let [function] = symbols.callables.functions() else {
        panic!(
            "expected exactly one `{name}`, found {}",
            symbols.callables.functions().len()
        );
    };
    function.clone()
}

#[test]
fn internal_friend_access_follows_the_selected_archive_not_the_package() {
    let mut friend_only = function("friendOnly", None, Vec::new());
    friend_only.visibility = Visibility::Internal;
    let mut ordinary_only = function("ordinaryOnly", None, Vec::new());
    ordinary_only.visibility = Visibility::Internal;
    let shared = segments(&["shared"]);
    let inventory = inventory::PackageInventory::from_package_inputs(vec![
        (
            shared.clone(),
            package_of(vec![friend_only]),
            false,
            true,
            Some("friend".to_string()),
        ),
        (
            shared,
            package_of(vec![ordinary_only]),
            false,
            false,
            Some("ordinary".to_string()),
        ),
    ])
    .expect("both package fragments are signable");
    let libraries = KlibLibraries::over(Rc::new(inventory));

    let friend = single_function(&libraries, "shared", "friendOnly");
    let ordinary = single_function(&libraries, "shared", "ordinaryOnly");
    let platform = &libraries as &dyn crate::libraries::SemanticPlatform;
    assert!(platform
        .dependency_access_policy()
        .internal_callable_accessible(friend.callable.external_identity, friend.callable.owner,));
    assert!(!platform
        .dependency_access_policy()
        .internal_callable_accessible(
            ordinary.callable.external_identity,
            ordinary.callable.owner,
        ));
    assert_eq!(friend.callable.owner, ordinary.callable.owner);
}

#[test]
fn a_top_level_function_resolves_with_its_semantic_signature() {
    let libraries = kotlin_io();
    let println = single_function(&libraries, "kotlin/io", "println");

    let kotlin_io = SemanticCallableOwner::Package(type_name("kotlin/io"));
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    assert_eq!(println.kind, FnKind::TopLevel);
    assert_eq!(println.semantic_params().as_ref(), [any]);
    assert_eq!(println.semantic_receiver(), None);
    assert_eq!(println.callable.name, "println");
    assert_eq!(println.callable.params, vec![any]);
    assert_eq!(println.callable.ret, Ty::Unit);
    assert_eq!(println.callable.declaration_owner, Some(kotlin_io));
    assert_eq!(println.visibility, Visibility::Public);
    assert_eq!(println.context_count, 0);
}

#[test]
fn a_top_level_function_publishes_its_return_value_status() {
    let mut declaration = function("answer", None, Vec::new());
    declaration.return_value_status = crate::types::ReturnValueStatus::MustUse;
    let libraries =
        KlibLibraries::from_packages(vec![(segments(&["p"]), package_of(vec![declaration]))])
            .expect("the function is signable");
    let answer = single_function(&libraries, "p", "answer");
    assert_eq!(
        answer.flags.return_value_status,
        Some(crate::types::ReturnValueStatus::MustUse)
    );
}

#[test]
fn a_klib_function_publishes_context_and_value_inline_modifiers() {
    use crate::types::InlineParameterModifier::{Crossinline, Noinline, None as Plain};

    let mut declaration = function(
        "invoke",
        None,
        vec![
            class("kotlin/Function0", false),
            class("kotlin/Function0", false),
            class("kotlin/Function0", false),
        ],
    );
    declaration.is_inline = true;
    declaration.context_count = 1;
    declaration.context_kinds = vec![ContextParameterKind::Named];
    declaration.inline_modifiers = vec![Crossinline, Plain, Noinline];
    let libraries =
        KlibLibraries::from_packages(vec![(segments(&["p"]), package_of(vec![declaration]))])
            .expect("the function is signable");

    let function = single_function(&libraries, "p", "invoke");
    assert_eq!(
        function.call_sig.inline_modifiers,
        [Crossinline, Plain, Noinline]
    );
    assert_eq!(
        function.callable.inline_modifiers.as_ref(),
        [Crossinline, Plain, Noinline]
    );
}

#[test]
fn the_selected_identity_realizes_the_exact_public_signature() {
    let libraries = kotlin_io();
    let println = single_function(&libraries, "kotlin/io", "println");
    let identity = println
        .callable
        .external_identity
        .expect("a published function carries its provider identity");

    let realization = libraries
        .external_callable(identity)
        .expect("the provider answers for the identity it assigned");
    assert_eq!(realization.kind, ExternalCallableKind::TopLevel);
    assert_eq!(
        realization.declaration_owner,
        Some(SemanticCallableOwner::Package(type_name("kotlin/io")))
    );
    assert_eq!(realization.callable.external_identity, Some(identity));
    assert_eq!(
        realization.declaration_signature,
        Some(expected_signature(&["kotlin", "io"], &self::println()))
    );
    let Some(KlibDeclarationSignature::Public(signature)) = realization.declaration_signature
    else {
        panic!("a top-level function realizes a public signature");
    };
    assert_eq!(signature.package().segments(), ["kotlin", "io"]);
    assert_eq!(signature.declaration().segments(), ["println"]);
    assert_eq!(
        signature.member_id().map(|id| id as i64),
        Some(-3_363_048_611_743_956_379)
    );
    assert_eq!(signature.mask(), 0);
}

#[test]
fn a_repeated_lookup_returns_the_same_identity() {
    let libraries = kotlin_io();
    let key = SymbolNamespace::Package(type_name("kotlin/io"));
    let first = libraries.symbols(key, "println");
    let second = libraries.symbols(key, "println");
    assert!(Rc::ptr_eq(&first, &second));
    let identity = |symbols: &crate::libraries::ResolvedSymbols| {
        symbols
            .callables
            .functions()
            .iter()
            .map(|function| function.callable.external_identity)
            .collect::<Vec<_>>()
    };
    assert_eq!(identity(&first), identity(&second));
    assert_eq!(identity(&first).len(), 1);
    assert!(libraries
        .external_callable(crate::fir::ExternalCallableId::from_raw(1))
        .is_none());
}

#[test]
fn declared_packages_and_their_parents_exist() {
    let libraries = kotlin_io();
    let kotlin = type_name("kotlin");
    assert!(libraries.package_exists(TypeName::ROOT, "kotlin"));
    assert!(libraries.package_exists(kotlin, "io"));
    assert!(!libraries.package_exists(kotlin, "klibProviderAbsentPackage"));
    assert!(!libraries.package_exists(TypeName::ROOT, "io"));
}

#[test]
fn an_extension_function_resolves_as_an_extension() {
    let int = class("kotlin/Int", false);
    let coerce = function("coerceAtLeast", Some(int.clone()), vec![int]);
    let libraries = KlibLibraries::from_packages(vec![(
        segments(&["kotlin", "ranges"]),
        package_of(vec![coerce]),
    )])
    .expect("an in-memory kotlin.ranges package is signable");

    let function = single_function(&libraries, "kotlin/ranges", "coerceAtLeast");
    assert_eq!(function.kind, FnKind::Extension);
    assert_eq!(function.semantic_receiver(), Some(Ty::Int));
    assert_eq!(function.semantic_params().as_ref(), [Ty::Int]);
    assert_eq!(function.callable.params, vec![Ty::Int, Ty::Int]);
    assert_eq!(function.callable.source_receiver, Some(Ty::Int));

    let realization = libraries
        .external_callable(function.callable.external_identity.expect("identity"))
        .expect("realization");
    assert_eq!(realization.kind, ExternalCallableKind::Extension);
    let Some(KlibDeclarationSignature::Public(signature)) = realization.declaration_signature
    else {
        panic!("an extension function realizes a public signature");
    };
    assert_eq!(
        signature.member_id().map(|id| id as i64),
        Some(3_998_717_805_095_061_419)
    );
}

#[test]
fn fragments_of_one_package_merge_and_private_functions_are_not_published() {
    let mut hidden = function("println", None, vec![class("kotlin/String", false)]);
    hidden.visibility = Visibility::Private;
    let libraries = KlibLibraries::from_packages(vec![
        (segments(&["kotlin", "io"]), package_of(vec![println()])),
        (segments(&["kotlin", "io"]), package_of(vec![hidden])),
        (
            segments(&["kotlin", "io"]),
            package_of(vec![function(
                "println",
                None,
                vec![class("kotlin/Int", false)],
            )]),
        ),
    ])
    .expect("signable fragments");

    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("kotlin/io")), "println");
    let params = symbols
        .callables
        .functions()
        .iter()
        .map(|function| function.semantic_params().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        params,
        vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))], vec![Ty::Int]]
    );
}

#[test]
fn duplicate_fragments_publish_one_candidate_and_one_realization() {
    let libraries = KlibLibraries::from_packages(vec![
        (segments(&["kotlin", "io"]), package_of(vec![println()])),
        (segments(&["kotlin", "io"]), package_of(vec![println()])),
    ])
    .expect("duplicate fragments contain one signable declaration");

    let println = single_function(&libraries, "kotlin/io", "println");
    let identity = println
        .callable
        .external_identity
        .expect("the one candidate has one provider identity");
    let realization = libraries
        .external_callable(identity)
        .expect("the one identity has one realization");
    assert_eq!(realization.callable.external_identity, Some(identity));
    assert!(libraries
        .external_callable(crate::fir::ExternalCallableId::from_raw(1))
        .is_none());
}

#[test]
fn an_unsignable_declaration_rejects_the_library_set() {
    let mut malformed = println();
    malformed.context_count = 2;
    let error = KlibLibraries::from_packages(vec![(
        segments(&["kotlin", "io"]),
        package_of(vec![malformed]),
    )])
    .err()
    .expect("a declaration without an identity cannot be published");
    assert_eq!(error.package(), ["kotlin", "io"]);
    assert_eq!(error.declaration(), "println");
    assert_eq!(
        error.detail(),
        "function println has 2 context parameters but 1 parameters"
    );
    assert_eq!(
        error.to_string(),
        "cannot sign KLIB declaration println in package `kotlin.io`: \
         function println has 2 context parameters but 1 parameters"
    );
}

fn realized_parameters(
    libraries: &KlibLibraries,
    function: &FunctionInfo,
) -> Box<[ResolvedParameterIdentity]> {
    libraries
        .external_callable(function.callable.external_identity.expect("identity"))
        .expect("realization")
        .parameter_identities
}

#[test]
fn a_klib_function_is_published_without_a_compiler_intrinsic() {
    let mut greet = function("greet", None, vec![class("kotlin/Any", true)]);
    greet.param_names = vec!["message".to_owned()];
    let libraries =
        KlibLibraries::from_packages(vec![(segments(&["kotlin", "io"]), package_of(vec![greet]))])
            .expect("an in-memory kotlin.io package is signable");
    let greet = single_function(&libraries, "kotlin/io", "greet");
    assert_eq!(greet.callable.compiler_intrinsic, None);
    assert_eq!(greet.callable.semantic_role, None);
    let realization = libraries
        .external_callable(greet.callable.external_identity.expect("identity"))
        .expect("realization");
    assert_eq!(realization.callable.compiler_intrinsic, None);
    assert_eq!(realization.callable.semantic_role, None);
}

#[test]
fn a_builtin_klib_function_carries_its_shared_compiler_operation() {
    let libraries = kotlin_io();
    let println = single_function(&libraries, "kotlin/io", "println");
    assert_eq!(
        println.callable.compiler_intrinsic,
        Some(crate::libraries::CompilerIntrinsic::Println)
    );
    assert_eq!(println.callable.semantic_role, None);
}

#[test]
fn a_dependency_closed_range_lookalike_keeps_its_serialized_body() {
    let mut contains = function(
        "contains",
        Some(class("kotlin/ranges/IntRange", false)),
        vec![class("kotlin/Char", false)],
    );
    contains.ret = class("kotlin/Boolean", false);
    contains.is_operator = true;
    let libraries = KlibLibraries::from_packages(vec![(
        segments(&["kotlin", "ranges"]),
        package_of(vec![contains]),
    )])
    .expect("the same-package dependency is signable");

    let contains = single_function(&libraries, "kotlin/ranges", "contains");
    assert_eq!(contains.callable.compiler_intrinsic, None);
    let realization = libraries
        .external_callable(contains.callable.external_identity.expect("identity"))
        .expect("the dependency body remains its realization");
    assert_eq!(realization.callable.compiler_intrinsic, None);
}

fn write_closed_range_contains_klib(
    label: &str,
    unique_name: &str,
    operator: bool,
    ret: Ty,
    defaulted: bool,
) -> std::path::PathBuf {
    write_closed_range_contains_klib_with_shadowed_declarations(
        label,
        unique_name,
        operator,
        ret,
        defaulted,
        false,
    )
}

fn write_closed_range_contains_klib_with_shadowed_declarations(
    label: &str,
    unique_name: &str,
    operator: bool,
    ret: Ty,
    defaulted: bool,
    shadowed_declarations: bool,
) -> std::path::PathBuf {
    let mut declaration = crate::metadata::builder::FnMeta::plain(
        "contains",
        vec![("value".to_string(), Ty::Char)],
        ret,
    );
    declaration.receiver = Some(Ty::obj("kotlin/ranges/IntRange"));
    declaration.operator = operator;
    declaration.param_modifiers = vec![crate::metadata::DeclaredValueParameter::defaulted(
        defaulted,
    )];
    let properties = shadowed_declarations
        .then(|| crate::metadata::builder::PropMeta {
            name: "dependencyOnlyProperty".to_owned(),
            ty: Ty::String,
            is_var: false,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            receiver: None,
            context_params: Vec::new(),
            getter: None,
            setter: None,
            setter_parameter_name: None,
            is_const: false,
            has_constant: false,
            decl_order: 1,
            visibility: Visibility::Public,
            spellings: Default::default(),
            has_backing_field: true,
            modifiers: Default::default(),
            setter_visibility: Visibility::Public,
            declares_annotations: false,
            annotation_method: None,
            companion: false,
            accessor_annotations: Default::default(),
            field_name: None,
            field_desc: None,
            delegate_method: None,
        })
        .into_iter()
        .collect::<Vec<_>>();
    let class_tail = crate::metadata::class_builder::ClassTail::default();
    let dependency_only_class = crate::metadata::class_builder::ClassDeclaration {
        name: type_name("kotlin/ranges/DependencyOnlyClass"),
        ctor_params: &[],
        props: &[],
        methods: &[],
        enum_entries: &[],
        tail: &class_tail,
    };
    let classes = shadowed_declarations
        .then(|| crate::metadata::klib_fragment::KlibClass {
            declaration: dependency_only_class,
            constants: &[],
            in_source_file: true,
        })
        .into_iter()
        .collect::<Vec<_>>();
    let fragment = crate::metadata::klib_fragment::package_fragment(
        &["kotlin", "ranges"],
        &crate::metadata::klib_fragment::KlibFileMembers {
            file_name: "dependency.kt".to_string(),
            functions: vec![declaration],
            constants: vec![None; properties.len()],
            properties,
            aliases: Vec::new(),
        },
        &classes,
        true,
    );
    let mut writer = crate::klib::write::KlibWriter::native_test_fixture(unique_name);
    writer.add_fragment("kotlin.ranges", fragment);
    let directory = std::env::temp_dir().join(format!(
        "krusty-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    writer
        .write_directory(&directory)
        .expect("write range-contains KLIB");
    directory
}

#[test]
fn a_metadata_only_klib_publishes_declarations_without_inventing_default_values() {
    let directory =
        write_closed_range_contains_klib("metadata-only", "metadata-only", true, Ty::Boolean, true);
    let libraries = KlibLibraries::open(&[KlibLibraryInput::dependency(directory.clone())])
        .expect("the metadata-only dependency opens");
    let contains = single_function(&libraries, "kotlin/ranges", "contains");
    assert_eq!(contains.call_sig.param_defaults, [true]);
    assert_eq!(contains.default_values, []);

    drop(libraries);
    std::fs::remove_dir_all(directory).expect("remove metadata-only dependency");
}

#[test]
fn a_partial_serialized_ir_channel_remains_invalid() {
    let directory =
        write_closed_range_contains_klib("partial-ir", "partial-ir", true, Ty::Boolean, false);
    let ir = directory.join("default/ir");
    std::fs::create_dir_all(&ir).expect("create partial IR directory");
    std::fs::write(ir.join("files.knf"), [0u8; 4]).expect("write the one partial IR table");
    let error = KlibLibraries::open(&[KlibLibraryInput::dependency(directory.clone())])
        .err()
        .expect("a partial serialized IR channel is invalid");
    assert_eq!(
        error.to_string(),
        format!(
            "cannot decode the KLIB IR declarations of {}: the KLIB has no serialized IR at default/ir/strings.knt",
            directory.display()
        )
    );

    std::fs::remove_dir_all(directory).expect("remove partial-IR dependency");
}

#[test]
fn a_dependency_cannot_claim_stdlib_authority_from_its_manifest() {
    let directory =
        write_closed_range_contains_klib("spoofed-stdlib", "stdlib", true, Ty::Boolean, false);

    let libraries = KlibLibraries::open(&[KlibLibraryInput::dependency(directory.clone())])
        .expect("the dependency archive opens");
    let contains = single_function(&libraries, "kotlin/ranges", "contains");
    assert_eq!(contains.callable.compiler_intrinsic, None);
    assert_eq!(
        libraries
            .external_callable(contains.callable.external_identity.expect("identity"))
            .expect("dependency realization")
            .callable
            .compiler_intrinsic,
        None
    );

    drop(libraries);
    std::fs::remove_dir_all(directory).expect("remove spoofed stdlib dependency");
}

#[test]
fn a_platform_body_snapshot_canonicalizes_equivalent_archive_paths() {
    let directory =
        write_closed_range_contains_klib("canonical-stdlib", "stdlib", true, Ty::Boolean, false);
    let direct = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        directory.clone(),
        KlibStdlibSemantics::Native,
    )])
    .expect("the direct platform path opens");
    let dotted = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        directory.join("."),
        KlibStdlibSemantics::Native,
    )])
    .expect("the equivalent dotted platform path opens");

    assert_eq!(
        direct.dependency_body_snapshot(),
        dotted.dependency_body_snapshot()
    );

    drop(direct);
    drop(dotted);
    std::fs::remove_dir_all(directory).expect("remove canonical stdlib fixture");
}

#[test]
fn a_platform_body_snapshot_changes_when_the_archive_is_rebuilt_in_place() {
    let directory = write_empty_klib("rebuilt-stdlib", "stdlib");
    let first = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        directory.clone(),
        KlibStdlibSemantics::Native,
    )])
    .expect("the first archive generation opens");

    let manifest_path = directory.join("default/manifest");
    let mut manifest = std::fs::read_to_string(&manifest_path).expect("read manifest");
    manifest.push_str("generation=second\n");
    std::fs::write(&manifest_path, manifest).expect("replace archive contents in place");
    let second = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        directory.clone(),
        KlibStdlibSemantics::Native,
    )])
    .expect("the second archive generation opens");

    assert_ne!(
        first.dependency_body_snapshot(),
        second.dependency_body_snapshot(),
        "equal paths and version stamps cannot hide changed archive contents"
    );

    drop(first);
    drop(second);
    std::fs::remove_dir_all(directory).expect("remove rebuilt stdlib fixture");
}

#[test]
fn publication_rejects_a_generation_replaced_after_its_bytes_were_frozen() {
    let directory = write_empty_klib("changed-during-open", "stdlib");
    let opened =
        crate::klib::KlibArchive::open(&directory).expect("freeze the initial archive generation");

    let manifest_path = directory.join("default/manifest");
    let mut manifest = std::fs::read_to_string(&manifest_path).expect("read manifest");
    manifest.push_str("generation=replaced-before-publication\n");
    std::fs::write(&manifest_path, manifest).expect("replace the selected generation");

    assert!(
        !super::archives::opened_generation_is_current(&directory, &opened)
            .expect("compare the selected semantic generations"),
        "a provider may decode its frozen bytes but must not publish them under a replaced path"
    );

    std::fs::remove_dir_all(directory).expect("remove generation-publication fixture");
}

fn write_empty_klib(label: &str, unique_name: &str) -> std::path::PathBuf {
    let writer = crate::klib::write::KlibWriter::native_test_fixture(unique_name);
    let directory = std::env::temp_dir().join(format!(
        "krusty-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    writer
        .write_directory(&directory)
        .expect("write empty KLIB");
    directory
}

#[test]
fn a_metadata_only_producer_edge_is_part_of_the_body_snapshot() {
    let platform = write_empty_klib("edge-platform", "stdlib");
    let platform_manifest = platform.join("default/manifest");
    let mut manifest = std::fs::read_to_string(&platform_manifest).expect("read platform manifest");
    manifest.push_str("depends=bridge\n");
    std::fs::write(&platform_manifest, manifest).expect("record bridge dependency");

    let first_bridge = write_empty_klib("edge-bridge-first", "bridge");
    let second_bridge = write_empty_klib("edge-bridge-second", "bridge");
    let second_manifest = second_bridge.join("default/manifest");
    let manifest = std::fs::read_to_string(&second_manifest)
        .expect("read second bridge manifest")
        .replace("compiler_version=2.4.20", "compiler_version=2.4.21");
    assert!(manifest.contains("compiler_version=2.4.21"));
    std::fs::write(&second_manifest, manifest).expect("change bridge version");

    let first = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(platform.clone(), KlibStdlibSemantics::Native),
        KlibLibraryInput::dependency(first_bridge.clone()),
    ])
    .expect("the first producer graph opens");
    let second = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(platform.clone(), KlibStdlibSemantics::Native),
        KlibLibraryInput::dependency(second_bridge.clone()),
    ])
    .expect("the changed producer graph opens");

    assert_ne!(
        first.dependency_body_snapshot(),
        second.dependency_body_snapshot(),
        "an edge-only archive can change the reachable body graph"
    );

    drop(first);
    drop(second);
    std::fs::remove_dir_all(platform).expect("remove edge platform fixture");
    std::fs::remove_dir_all(first_bridge).expect("remove first bridge fixture");
    std::fs::remove_dir_all(second_bridge).expect("remove second bridge fixture");
}

#[test]
fn a_dependency_first_duplicate_cannot_hide_the_designated_stdlib_role() {
    // Public KLIB signatures intentionally omit result type and source modifiers. Give the
    // dependency the same serialized identity but incompatible metadata so this proves the full
    // authoritative declaration replaces it rather than merely receiving the stdlib authority bit.
    let dependency = write_closed_range_contains_klib(
        "duplicate-dependency",
        "dependency",
        false,
        Ty::String,
        false,
    );
    let stdlib =
        write_closed_range_contains_klib("designated-stdlib", "stdlib", true, Ty::Boolean, false);
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::dependency(dependency.clone()),
        KlibLibraryInput::platform_stdlib(stdlib.clone(), KlibStdlibSemantics::Native),
    ])
    .expect("the duplicate dependency and designated stdlib open");

    let contains = single_function(&libraries, "kotlin/ranges", "contains");
    let expected = Some(crate::libraries::CompilerIntrinsic::ClosedRangeContains);
    assert!(contains.flags.operator);
    assert_eq!(contains.callable.ret, Ty::Boolean);
    assert_eq!(contains.callable.compiler_intrinsic, expected);
    assert_eq!(
        libraries
            .external_callable(contains.callable.external_identity.expect("identity"))
            .expect("selected realization")
            .callable
            .compiler_intrinsic,
        expected
    );

    drop(libraries);
    std::fs::remove_dir_all(dependency).expect("remove duplicate dependency");
    std::fs::remove_dir_all(stdlib).expect("remove designated stdlib");
}

#[test]
fn a_later_dependency_with_the_same_module_name_cannot_replace_the_designated_stdlib() {
    let stdlib =
        write_closed_range_contains_klib("designated-stdlib", "shared", true, Ty::Boolean, false);
    let dependency =
        write_closed_range_contains_klib("later-dependency", "shared", false, Ty::String, false);
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(stdlib.clone(), KlibStdlibSemantics::Native),
        KlibLibraryInput::dependency(dependency.clone()),
    ])
    .expect("the designated stdlib and same-name dependency open");

    let contains = single_function(&libraries, "kotlin/ranges", "contains");
    let expected = Some(crate::libraries::CompilerIntrinsic::ClosedRangeContains);
    assert!(contains.flags.operator);
    assert_eq!(contains.callable.ret, Ty::Boolean);
    assert_eq!(contains.callable.compiler_intrinsic, expected);

    drop(libraries);
    std::fs::remove_dir_all(stdlib).expect("remove designated stdlib");
    std::fs::remove_dir_all(dependency).expect("remove later dependency");
}

#[test]
fn a_same_module_dependency_publishes_no_declarations_beside_the_designated_stdlib() {
    let stdlib =
        write_closed_range_contains_klib("authority-stdlib", "shared", true, Ty::Boolean, false);
    let dependency = write_closed_range_contains_klib_with_shadowed_declarations(
        "authority-dependency",
        "shared",
        false,
        Ty::String,
        false,
        true,
    );

    for dependency_first in [true, false] {
        let inputs = if dependency_first {
            [
                KlibLibraryInput::dependency(dependency.clone()),
                KlibLibraryInput::platform_stdlib(stdlib.clone(), KlibStdlibSemantics::Native),
            ]
        } else {
            [
                KlibLibraryInput::platform_stdlib(stdlib.clone(), KlibStdlibSemantics::Native),
                KlibLibraryInput::dependency(dependency.clone()),
            ]
        };
        let libraries = KlibLibraries::open(&inputs)
            .expect("the designated stdlib exclusively owns its module record");

        let contains = single_function(&libraries, "kotlin/ranges", "contains");
        assert!(contains.flags.operator);
        assert_eq!(contains.callable.ret, Ty::Boolean);
        assert_eq!(
            contains.callable.compiler_intrinsic,
            Some(crate::libraries::CompilerIntrinsic::ClosedRangeContains)
        );
        let property = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlin/ranges")),
            "dependencyOnlyProperty",
        );
        assert!(property.callables.properties().is_empty());
        assert!(property.callables.functions().is_empty());
        assert!(property.classifier.is_none());
        let classifier = libraries.symbols(
            SymbolNamespace::Package(type_name("kotlin/ranges")),
            "DependencyOnlyClass",
        );
        assert!(classifier.classifier.is_none());
        assert!(classifier.classifier_declaration.is_none());
        assert!(classifier.is_empty());
    }

    std::fs::remove_dir_all(stdlib).expect("remove authority stdlib");
    std::fs::remove_dir_all(dependency).expect("remove authority dependency");
}

#[test]
fn multiple_typed_platform_stdlibs_are_rejected_before_archive_decoding() {
    let error = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib("first-stdlib.klib".into(), KlibStdlibSemantics::Native),
        KlibLibraryInput::platform_stdlib("second-stdlib.klib".into(), KlibStdlibSemantics::Native),
    ])
    .err()
    .expect("multiple selected platform stdlibs are invalid");
    assert_eq!(
        error.to_string(),
        "multiple platform stdlib KLIB inputs: first-stdlib.klib and second-stdlib.klib"
    );
}

#[test]
fn a_klib_delegate_function_keeps_its_serialized_body_authoritative() {
    let libraries = KlibLibraries::from_packages(vec![(
        segments(&["kotlin"]),
        package_of(vec![property_reference_get_value()]),
    )])
    .expect("the delegate declaration is signable");
    let get_value = single_function(&libraries, "kotlin", "getValue");
    assert_eq!(get_value.callable.compiler_intrinsic, None);
    assert_eq!(get_value.callable.semantic_role, None);

    let realization = libraries
        .external_callable(get_value.callable.external_identity.expect("identity"))
        .expect("realization");
    assert_eq!(realization.callable.compiler_intrinsic, None);
    assert_eq!(realization.callable.semantic_role, None);
    assert_eq!(
        realization.declaration_signature,
        Some(expected_signature(
            &["kotlin"],
            &property_reference_get_value()
        ))
    );
}

#[test]
fn same_spelled_function_and_property_type_parameters_keep_distinct_identities() {
    let parameter = |id| KotlinTypeParameter {
        id: KotlinTypeParameterId(id),
        name: "T".to_owned(),
        bounds: Vec::new(),
        variance: TypeVariance::Invariant,
        only_input: false,
        reified: false,
    };
    let parameter_ty = |id| KotlinType::Param {
        name: "T".to_owned(),
        id: KotlinTypeParameterId(id),
        nullable: false,
    };
    let mut identity = function("identity", None, vec![parameter_ty(11)]);
    identity.ret = parameter_ty(11);
    identity.formals = vec![parameter(11)];
    identity.contract = Some(std::sync::Arc::new(crate::contracts::Contract {
        effects: vec![crate::contracts::Effect::ConditionalReturns {
            returns: crate::contracts::ReturnsValue::Any,
            conclusion: crate::contracts::Condition::IsType {
                param: crate::contracts::ParamRef::Param(0),
                ty: crate::contracts::ConditionType::Metadata(Ty::ty_param(
                    "T",
                    Ty::nullable(Ty::obj("kotlin/Any")),
                )),
                negated: false,
            },
        }],
    }));
    let property = KotlinProperty {
        name: "last".to_owned(),
        receiver: Some(KotlinType::Class {
            internal: "kotlin/collections/List".to_owned(),
            args: vec![parameter_ty(37)],
            nullable: false,
            shape: KotlinFunctionTypeShape::default(),
        }),
        context_params: Vec::new(),
        ty: parameter_ty(37),
        formals: vec![parameter(37)],
        visibility: Visibility::Public,
        modality: crate::metadata::semantic::KotlinModality::Final,
        getter_modifiers: crate::metadata::semantic::KotlinAccessorModifiers::default_for(
            Visibility::Public,
            crate::metadata::semantic::KotlinModality::Final,
        ),
        setter_modifiers: crate::metadata::semantic::KotlinAccessorModifiers::default_for(
            Visibility::Public,
            crate::metadata::semantic::KotlinModality::Final,
        ),
        is_external: false,
        setter_parameter_name: None,
        is_var: false,
        is_const: false,
        is_expect: false,
        is_static: false,
        context_count: 0,
        context_param_names: Vec::new(),
        context_kinds: Vec::new(),
        constant: None,
        annotations: Vec::new(),
        return_value_status: Default::default(),
    };
    let libraries = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        KotlinPackage {
            functions: vec![identity],
            properties: vec![property],
            ..KotlinPackage::default()
        },
    )])
    .expect("both generic declarations are signable");

    let identity = single_function(&libraries, "p", "identity");
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("p")), "last");
    let [property] = symbols.callables.properties() else {
        panic!("expected one generic property");
    };
    let function_formal = identity
        .generic_sig
        .as_ref()
        .expect("generic function signature")
        .formals[0]
        .as_str();
    let property_formal = property.formals[0].as_str();
    assert_ne!(function_formal, property_formal);
    assert_eq!(
        crate::types::type_parameter_source_name(function_formal),
        "T"
    );
    assert_eq!(
        crate::types::type_parameter_source_name(property_formal),
        "T"
    );
    assert_eq!(
        identity.semantic_params().as_ref(),
        [Ty::ty_param(
            function_formal,
            Ty::nullable(Ty::obj("kotlin/Any"))
        )]
    );
    assert_eq!(
        identity.callable.ret,
        Ty::ty_param(function_formal, Ty::nullable(Ty::obj("kotlin/Any")))
    );
    assert_eq!(
        property.ty,
        Ty::ty_param(property_formal, Ty::nullable(Ty::obj("kotlin/Any")))
    );
    assert_eq!(
        property.receiver.expect("extension receiver").type_args(),
        [property.ty]
    );
    let contract = identity
        .callable
        .contract
        .as_deref()
        .expect("the provider publishes the function contract");
    let [crate::contracts::Effect::ConditionalReturns { conclusion, .. }] =
        contract.effects.as_slice()
    else {
        panic!("expected the generic conditional return effect");
    };
    let crate::contracts::Condition::IsType {
        ty: crate::contracts::ConditionType::Metadata(conclusion_ty),
        ..
    } = conclusion
    else {
        panic!("expected the generic is-type conclusion");
    };
    let TyKind::TyParam(contract_formal, _) = conclusion_ty.kind() else {
        panic!("expected the generic is-type conclusion");
    };
    assert_eq!(contract_formal, function_formal);
}

#[test]
fn a_value_parameter_keeps_its_source_name() {
    let libraries = kotlin_io();
    let println = single_function(&libraries, "kotlin/io", "println");
    let message = ResolvedParameterIdentity::Source("message".into());
    assert_eq!(
        println.call_sig.parameter_identities,
        std::slice::from_ref(&message)
    );
    assert_eq!(*realized_parameters(&libraries, &println), [message]);
}

#[test]
fn legacy_context_receivers_precede_the_extension_receiver_and_the_values() {
    let int = class("kotlin/Int", false);
    let mut legacy = function(
        "legacy",
        Some(int.clone()),
        vec![
            class("kotlin/String", false),
            class("kotlin/Long", false),
            int,
        ],
    );
    legacy.param_names = vec![String::new(), String::new(), "count".to_owned()];
    legacy.context_count = 2;
    legacy.context_kinds = vec![ContextParameterKind::LegacyReceiver; 2];
    let libraries =
        KlibLibraries::from_packages(vec![(segments(&["p"]), package_of(vec![legacy]))])
            .expect("a consistent declaration is published");

    let function = single_function(&libraries, "p", "legacy");
    assert_eq!(
        function.call_sig.parameter_identities,
        [
            ResolvedParameterIdentity::LegacyContextReceiver { ordinal: 0 },
            ResolvedParameterIdentity::LegacyContextReceiver { ordinal: 1 },
            ResolvedParameterIdentity::Source("count".into()),
        ]
    );
    assert_eq!(
        *realized_parameters(&libraries, &function),
        [
            ResolvedParameterIdentity::LegacyContextReceiver { ordinal: 0 },
            ResolvedParameterIdentity::LegacyContextReceiver { ordinal: 1 },
            ResolvedParameterIdentity::ExtensionReceiver,
            ResolvedParameterIdentity::Source("count".into()),
        ]
    );
}

#[test]
fn named_and_anonymous_context_parameters_keep_their_roles() {
    let mut contextual = function(
        "contextual",
        None,
        vec![
            class("kotlin/String", false),
            class("kotlin/Long", false),
            class("kotlin/Int", false),
        ],
    );
    contextual.param_names = vec!["text".to_owned(), "_".to_owned(), "count".to_owned()];
    contextual.context_count = 2;
    contextual.context_kinds = vec![ContextParameterKind::Named, ContextParameterKind::Anonymous];
    let libraries =
        KlibLibraries::from_packages(vec![(segments(&["p"]), package_of(vec![contextual]))])
            .expect("a consistent declaration is published");

    let function = single_function(&libraries, "p", "contextual");
    let expected = [
        ResolvedParameterIdentity::ContextValue {
            ordinal: 0,
            source_name: "text".into(),
        },
        ResolvedParameterIdentity::AnonymousContextParameter { ordinal: 1 },
        ResolvedParameterIdentity::Source("count".into()),
    ];
    assert_eq!(function.call_sig.parameter_identities, expected);
    assert_eq!(*realized_parameters(&libraries, &function), expected);
}

#[test]
fn a_declaration_without_a_role_per_context_parameter_is_rejected() {
    let mut contextual = function("contextual", None, vec![class("kotlin/String", false)]);
    contextual.context_count = 1;
    let error =
        KlibLibraries::from_packages(vec![(segments(&["p"]), package_of(vec![contextual]))])
            .err()
            .expect("a declaration with inconsistent parameters cannot be published");
    assert_eq!(
        error.to_string(),
        "cannot sign KLIB declaration contextual in package `p`: \
         function contextual declares 1 context parameters with 0 roles among 1 parameters"
    );
}

mod analysis;
mod builtin_operations;
mod classes;
mod implementation_coverage;
mod implementation_inventory;
mod properties;
mod serialized_references;
