use std::rc::Rc;

use super::*;
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::{ExternalCallableKind, FnKind, FunctionInfo, KlibDeclarationSignature};
use crate::metadata::id_signature::{package_function_signature, MetadataContainer};
use crate::metadata::semantic::{
    KotlinFunction, KotlinFunctionTypeShape, KotlinProperty, KotlinType, KotlinTypeParameter,
    KotlinTypeParameterId,
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
        modality: crate::metadata::semantic::KotlinModality::Final,
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
                formals: vec!["T".to_string()],
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
                target,
                formals: vec!["T".to_string()],
                expansion,
                expansion_spelling: Default::default(),
            }
        ))
    );
    assert_eq!(
        <KlibLibraries as crate::libraries::SemanticPlatform>::type_alias_expansion(
            &libraries, identity,
        ),
        Some(crate::libraries::AliasExpansion {
            identity,
            target,
            formals: vec!["T".to_string()],
            expansion,
            expansion_spelling: Default::default(),
        })
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
        setter_visibility: Visibility::Public,
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
        ty: crate::contracts::ConditionType::Metadata(Ty::TyParam(contract_formal, _)),
        ..
    } = conclusion
    else {
        panic!("expected the generic is-type conclusion");
    };
    assert_eq!(*contract_formal, function_formal);
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
mod classes;
mod properties;
