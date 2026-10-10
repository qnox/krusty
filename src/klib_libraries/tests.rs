use std::rc::Rc;

use super::*;
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::{ExternalCallableKind, FnKind, FunctionInfo, KlibDeclarationSignature};
use crate::metadata::id_signature::{package_function_signature, MetadataContainer};
use crate::metadata::semantic::{
    KotlinFunction, KotlinFunctionTypeShape, KotlinType, KotlinTypeParameter, KotlinTypeParameterId,
};
use crate::types::{
    type_name, ContextParameterKind, SemanticCallableOwner, Ty, TypeVariance, Visibility,
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
        params,
        ret: class("kotlin/Unit", false),
        formals: Vec::new(),
        vararg: None,
        visibility: Visibility::Public,
        is_inline: false,
        has_reified_type_params: false,
        is_suspend: false,
        is_operator: false,
        is_infix: false,
        is_expect: false,
        is_static: false,
        context_count: 0,
        context_kinds: Vec::new(),
        annotations: Vec::new(),
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
    let libraries = kotlin_io();
    let println = single_function(&libraries, "kotlin/io", "println");
    assert_eq!(println.callable.compiler_intrinsic, None);
    assert_eq!(println.callable.semantic_role, None);
    let realization = libraries
        .external_callable(println.callable.external_identity.expect("identity"))
        .expect("realization");
    assert_eq!(realization.callable.compiler_intrinsic, None);
    assert_eq!(realization.callable.semantic_role, None);
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

mod properties;
