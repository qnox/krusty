//! Top-level properties published by the KLIB provider.

use std::collections::HashSet;
use std::path::Path;

use super::*;
use crate::klib::KlibArchive;
use crate::libraries::{
    ExternalPropertyRealization, LibConst, LibraryConst, PropKind, PropertyInfo, PropertyProducer,
    PropertyReadStability,
};
use crate::metadata::id_signature::{
    package_property_accessor_signature, package_property_signature, KlibAccessorIdSignature,
    MetadataAccessor,
};
use crate::metadata::klib_ir::tree::KlibIrMember;
use crate::metadata::klib_ir::{read_declaration_trees, KlibIrSignature};
use crate::metadata::semantic::{parse_package_fragment_checked, KotlinProperty};

fn property(name: &str, receiver: Option<KotlinType>, ty: KotlinType) -> KotlinProperty {
    KotlinProperty {
        name: name.to_owned(),
        receiver,
        context_params: Vec::new(),
        ty,
        formals: Vec::new(),
        visibility: Visibility::Public,
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
    }
}

fn package_of_properties(properties: Vec<KotlinProperty>) -> KotlinPackage {
    KotlinPackage {
        properties,
        ..KotlinPackage::default()
    }
}

fn libraries_of(package: &[&str], properties: Vec<KotlinProperty>) -> KlibLibraries {
    KlibLibraries::from_packages(vec![(segments(package), package_of_properties(properties))])
        .expect("an in-memory package of properties is signable")
}

/// `val <T> List<T>.lastIndex: Int` of `kotlin.collections`.
fn list_last_index() -> KotlinProperty {
    let parameter = KotlinTypeParameterId(0);
    let mut last_index = property(
        "lastIndex",
        Some(KotlinType::Class {
            internal: "kotlin/collections/List".to_owned(),
            args: vec![KotlinType::Param {
                name: "T".to_owned(),
                id: parameter,
                nullable: false,
            }],
            nullable: false,
            shape: KotlinFunctionTypeShape::default(),
        }),
        class("kotlin/Int", false),
    );
    last_index.formals = vec![KotlinTypeParameter {
        id: parameter,
        name: "T".to_owned(),
        bounds: Vec::new(),
        variance: TypeVariance::Invariant,
        only_input: false,
        reified: false,
    }];
    last_index
}

fn counter() -> KotlinProperty {
    let mut counter = property("counter", None, class("kotlin/Int", false));
    counter.is_var = true;
    counter
}

fn expected_accessor(
    package: &[&str],
    declaration: &KotlinProperty,
    accessor: MetadataAccessor,
) -> KlibAccessorIdSignature {
    let package = segments(package);
    let container = MetadataContainer {
        package: &package,
        classes: &[],
        native_interop_library: false,
    };
    package_property_accessor_signature(container, declaration, accessor)
        .expect("fixture is signable")
}

fn single_property(libraries: &KlibLibraries, package: &str, name: &str) -> PropertyInfo {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
    assert!(symbols.classifier.is_none());
    assert!(symbols.callables.functions().is_empty());
    let [property] = symbols.callables.properties() else {
        panic!(
            "expected exactly one `{name}`, found {}",
            symbols.callables.properties().len()
        );
    };
    property.clone()
}

fn property_realization(
    libraries: &KlibLibraries,
    property: &PropertyInfo,
) -> ExternalPropertyRealization {
    let identity = property
        .getter
        .external_property_identity
        .expect("a published property carries its provider identity");
    libraries
        .external_property(identity)
        .expect("the provider answers for the property identity it assigned")
}

fn accessor_realization(
    libraries: &KlibLibraries,
    identity: crate::fir::ExternalCallableId,
) -> crate::libraries::ExternalCallableRealization {
    libraries
        .external_callable(identity)
        .expect("the provider answers for the accessor identity it assigned")
}

fn accessor_signature(
    realization: &crate::libraries::ExternalCallableRealization,
) -> &KlibAccessorIdSignature {
    let Some(KlibDeclarationSignature::Accessor(signature)) = &realization.declaration_signature
    else {
        panic!("a property accessor realizes an accessor signature");
    };
    signature
}

#[test]
fn a_val_resolves_with_its_semantic_type_and_realizes_its_getter() {
    let libraries = libraries_of(
        &["p"],
        vec![property("answer", None, class("kotlin/Int", false))],
    );
    let answer = single_property(&libraries, "p", "answer");

    let package = SemanticCallableOwner::Package(type_name("p"));
    assert_eq!(answer.name, "answer");
    assert_eq!(answer.kind, PropKind::TopLevel);
    assert_eq!(answer.receiver, None);
    assert_eq!(answer.ty, Ty::Int);
    assert_eq!(answer.owner, type_name("p"));
    assert_eq!(answer.visibility, Visibility::Public);
    assert!(answer.formals.is_empty());
    assert_eq!(answer.context_count, 0);
    assert!(answer.context_parameter_identities.is_empty());
    assert!(!answer.is_const);
    assert_eq!(answer.compile_time_constant, None);
    assert_eq!(answer.producer, PropertyProducer::KotlinAccessor);
    assert_eq!(answer.read_stability, PropertyReadStability::Unstable);
    assert!(answer.setter.is_none());
    assert_eq!(answer.getter.name, "<get-answer>");
    assert_eq!(answer.getter.physical_name, None);
    assert_eq!(answer.getter.descriptor, "");
    assert!(answer.getter.params.is_empty());
    assert_eq!(answer.getter.ret, Ty::Int);
    assert_eq!(answer.getter.declaration_owner, Some(package));
    assert_eq!(answer.getter.generic_sig, None);

    let realization = property_realization(&libraries, &answer);
    assert_eq!(realization.name, "answer");
    assert_eq!(Some(realization.getter), answer.getter.external_identity);
    assert_eq!(realization.setter, None);
    assert!(!realization.declares_value_class_storage);
    assert_eq!(realization.compile_time_constant, None);

    let getter = accessor_realization(&libraries, realization.getter);
    assert_eq!(getter.kind, ExternalCallableKind::TopLevel);
    assert_eq!(getter.declaration_owner, Some(package));
    assert_eq!(getter.callable.external_identity, Some(realization.getter));
    assert!(getter.parameter_identities.is_empty());
    let expected = expected_accessor(
        &["p"],
        &property("answer", None, class("kotlin/Int", false)),
        MetadataAccessor::Getter,
    );
    assert_eq!(
        getter.declaration_signature,
        Some(KlibDeclarationSignature::Accessor(expected))
    );
    let signature = accessor_signature(&getter);
    assert_eq!(signature.name(), "<get-answer>");
    assert_eq!(signature.property().package().segments(), ["p"]);
    assert_eq!(signature.property().declaration().segments(), ["answer"]);
}

#[test]
fn a_var_realizes_its_getter_and_setter_signatures() {
    let package = ["fixture", "signatures"];
    let libraries = libraries_of(&package, vec![counter()]);
    let counter = single_property(&libraries, "fixture/signatures", "counter");

    let setter = counter.setter.as_ref().expect("a var has a setter");
    assert_eq!(setter.name, "<set-counter>");
    assert_eq!(setter.params, vec![Ty::Int]);
    assert_eq!(setter.ret, Ty::Unit);
    assert_eq!(setter.visibility, Visibility::Public);
    assert_eq!(counter.setter_visibility, Visibility::Public);
    assert_eq!(counter.setter_parameter_name, None);
    assert_eq!(
        setter.external_property_identity,
        counter.getter.external_property_identity
    );

    let realization = property_realization(&libraries, &counter);
    assert_eq!(Some(realization.getter), counter.getter.external_identity);
    assert_eq!(realization.setter, setter.external_identity);
    assert_ne!(Some(realization.getter), realization.setter);

    let getter = accessor_realization(&libraries, realization.getter);
    let setter = accessor_realization(&libraries, realization.setter.expect("setter identity"));
    assert_eq!(setter.kind, ExternalCallableKind::TopLevel);
    assert_eq!(
        *setter.parameter_identities,
        [ResolvedParameterIdentity::PropertySetterValue]
    );

    let expected_getter = expected_accessor(&package, &self::counter(), MetadataAccessor::Getter);
    let expected_setter = expected_accessor(&package, &self::counter(), MetadataAccessor::Setter);
    let getter = accessor_signature(&getter);
    let setter = accessor_signature(&setter);
    assert_eq!(getter, &expected_getter);
    assert_eq!(setter, &expected_setter);
    assert_eq!(getter.name(), "<get-counter>");
    assert_eq!(setter.name(), "<set-counter>");
    assert_eq!(getter.member_id(), expected_getter.member_id());
    assert_eq!(setter.member_id(), expected_setter.member_id());
    assert_ne!(getter.member_id(), setter.member_id());
    // Both accessors name the one property signature.
    assert_eq!(getter.property(), setter.property());
    let package = segments(&package);
    let container = MetadataContainer {
        package: &package,
        classes: &[],
        native_interop_library: false,
    };
    assert_eq!(
        getter.property(),
        &package_property_signature(container, &self::counter()).expect("signable")
    );
}

/// Member ids the Kotlin/Native 2.4.20 stdlib KLIB serializes for `List<T>.lastIndex`, pinned in
/// the id_signature tests.
#[test]
fn an_extension_property_takes_its_receiver_as_an_accessor_parameter() {
    let libraries = libraries_of(&["kotlin", "collections"], vec![list_last_index()]);
    let last_index = single_property(&libraries, "kotlin/collections", "lastIndex");

    let list = last_index
        .receiver
        .expect("an extension property has a receiver");
    assert_eq!(last_index.kind, PropKind::Extension);
    assert_eq!(
        list.non_null().obj_internal(),
        Some(type_name("kotlin/collections/List"))
    );
    assert_eq!(last_index.ty, Ty::Int);
    assert_eq!(last_index.formals, ["T"]);
    assert_eq!(last_index.getter.params, vec![list]);
    assert_eq!(last_index.getter.source_receiver, Some(list));
    let generic = last_index
        .getter
        .generic_sig
        .as_deref()
        .expect("a generic property's getter carries its signature");
    assert_eq!(generic.formals, ["T"]);
    assert_eq!(generic.receiver, Some(list));
    assert!(generic.params.is_empty());
    assert_eq!(generic.ret, Ty::Int);

    let realization = property_realization(&libraries, &last_index);
    let getter = accessor_realization(&libraries, realization.getter);
    assert_eq!(getter.kind, ExternalCallableKind::Extension);
    assert_eq!(
        *getter.parameter_identities,
        [ResolvedParameterIdentity::ExtensionReceiver]
    );
    let signature = accessor_signature(&getter);
    assert_eq!(signature.name(), "<get-lastIndex>");
    assert_eq!(
        signature.property().package().segments(),
        ["kotlin", "collections"]
    );
    assert_eq!(signature.property().declaration().segments(), ["lastIndex"]);
    assert_eq!(
        signature.property().member_id().map(|id| id as i64),
        Some(-7_238_914_123_027_933_299)
    );
    assert_eq!(signature.member_id() as i64, 1_631_619_787_052_076_373);
    assert_eq!(signature.mask(), 0);
}

#[test]
fn a_private_property_is_not_published() {
    let mut hidden = property("hidden", None, class("kotlin/Int", false));
    hidden.visibility = Visibility::Private;
    let libraries = libraries_of(&["p"], vec![hidden]);
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("p")), "hidden");
    assert!(symbols.callables.properties().is_empty());
    assert!(symbols.callables.functions().is_empty());
    assert!(libraries
        .external_property(crate::fir::ExternalPropertyId::from_raw(0))
        .is_none());
}

#[test]
fn duplicate_fragments_publish_one_property_and_one_realization() {
    let libraries = KlibLibraries::from_packages(vec![
        (segments(&["p"]), package_of_properties(vec![counter()])),
        (segments(&["p"]), package_of_properties(vec![counter()])),
    ])
    .expect("duplicate fragments contain one signable declaration");

    let counter = single_property(&libraries, "p", "counter");
    let identity = counter
        .getter
        .external_property_identity
        .expect("the one candidate has one property identity");
    assert_eq!(identity, crate::fir::ExternalPropertyId::from_raw(0));
    let realization = property_realization(&libraries, &counter);
    assert_eq!(
        (Some(realization.getter), realization.setter),
        (
            Some(crate::fir::ExternalCallableId::from_raw(0)),
            Some(crate::fir::ExternalCallableId::from_raw(1))
        )
    );
    assert!(libraries
        .external_property(crate::fir::ExternalPropertyId::from_raw(1))
        .is_none());
    assert!(libraries
        .external_callable(crate::fir::ExternalCallableId::from_raw(2))
        .is_none());
}

#[test]
fn an_unsignable_property_rejects_the_library_set() {
    let mut malformed = property(
        "capacity",
        Some(class("kotlin/Int", true)),
        class("kotlin/Int", false),
    );
    malformed.is_static = true;
    let error = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_of_properties(vec![malformed]),
    )])
    .err()
    .expect("a property without an identity cannot be published");
    assert_eq!(error.package(), ["p"]);
    assert_eq!(error.declaration(), "capacity");
    assert_eq!(
        error.to_string(),
        "cannot sign KLIB declaration capacity in package `p`: \
         companion extension capacity extends a type that is not a class"
    );
}

#[test]
fn a_property_without_a_role_per_context_parameter_is_rejected() {
    let mut contextual = property("contextual", None, class("kotlin/Int", false));
    contextual.context_params = vec![class("kotlin/String", false)];
    contextual.context_count = 1;
    let error = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_of_properties(vec![contextual]),
    )])
    .err()
    .expect("a property with inconsistent context parameters cannot be published");
    assert_eq!(
        error.to_string(),
        "cannot sign KLIB declaration contextual in package `p`: \
         property contextual declares 1 context parameters with 1 types, 0 roles and 0 names"
    );
}

#[test]
fn a_const_property_without_its_value_is_rejected() {
    let mut constant = property("LIMIT", None, class("kotlin/Int", false));
    constant.is_const = true;
    let error = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_of_properties(vec![constant]),
    )])
    .err()
    .expect("a const property without its value cannot be published");
    assert_eq!(
        error.to_string(),
        "cannot sign KLIB declaration LIMIT in package `p`: \
         const property LIMIT has no compile-time value"
    );
}

#[test]
fn a_const_property_keeps_its_compile_time_value_and_annotations() {
    let mut constant = property("LIMIT", None, class("kotlin/Int", false));
    constant.is_const = true;
    constant.constant = Some(LibConst::Int(42));
    let coercion = type_name("kotlin/internal/ImplicitIntegerCoercion");
    constant.annotations = vec![coercion];
    let libraries = libraries_of(&["p"], vec![constant]);
    let limit = single_property(&libraries, "p", "LIMIT");
    assert!(limit.implicit_integer_coercion);
    assert_eq!(limit.getter.annotations, [coercion]);
    let expected = Some(LibraryConst {
        ty: Ty::Int,
        value: LibConst::Int(42),
    });
    assert!(limit.is_const);
    assert_eq!(limit.compile_time_constant, expected);
    assert_eq!(
        property_realization(&libraries, &limit).compile_time_constant,
        expected
    );
}

#[test]
fn a_private_setter_keeps_its_visibility_and_a_named_setter_value_its_name() {
    let mut guarded = counter();
    guarded.setter_visibility = Visibility::Private;
    guarded.setter_parameter_name = Some("next".to_owned());
    let libraries = libraries_of(&["p"], vec![guarded]);
    let counter = single_property(&libraries, "p", "counter");
    let setter = counter.setter.as_ref().expect("a var has a setter");
    assert_eq!(counter.visibility, Visibility::Public);
    assert_eq!(counter.setter_visibility, Visibility::Private);
    assert_eq!(setter.visibility, Visibility::Private);
    assert_eq!(counter.setter_parameter_name.as_deref(), Some("next"));
    let realization = property_realization(&libraries, &counter);
    let setter = accessor_realization(&libraries, realization.setter.expect("setter identity"));
    assert_eq!(
        *setter.parameter_identities,
        [ResolvedParameterIdentity::Source("next".into())]
    );
}

#[test]
fn a_klib_property_is_published_without_a_compiler_intrinsic() {
    let libraries = libraries_of(&["p"], vec![counter()]);
    let counter = single_property(&libraries, "p", "counter");
    let setter = counter.setter.as_ref().expect("a var has a setter");
    for accessor in [&counter.getter, setter] {
        assert_eq!(accessor.compiler_intrinsic, None);
        assert_eq!(accessor.semantic_role, None);
    }
    let realization = property_realization(&libraries, &counter);
    for identity in [Some(realization.getter), realization.setter] {
        let accessor = accessor_realization(&libraries, identity.expect("accessor identity"));
        assert_eq!(accessor.callable.compiler_intrinsic, None);
        assert_eq!(accessor.callable.semantic_role, None);
    }
}

fn fixture_libraries() -> (KlibLibraries, HashSet<KlibAccessorIdSignature>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/klib_signatures/signatures.klib");
    let archive = KlibArchive::open(&path).expect("the signature fixture KLIB opens");
    let packages = archive
        .package_fragments()
        .into_iter()
        .map(|fragment| {
            let bytes = archive.read(&fragment.entry).expect("fragment reads");
            let package = parse_package_fragment_checked(&bytes).expect("fragment decodes");
            let segments = fragment
                .package_fqname
                .split('.')
                .map(str::to_owned)
                .collect();
            (segments, package)
        })
        .collect();
    let libraries = KlibLibraries::from_packages(packages).expect("the fixture is signable");
    let trees = read_declaration_trees(&archive).expect("the fixture's IR decodes");
    let mut accessors = HashSet::new();
    for tree in trees.trees() {
        let KlibIrMember::Property(property) = &tree.declaration else {
            continue;
        };
        for accessor in [property.getter, property.setter].into_iter().flatten() {
            if let KlibIrSignature::Accessor(signature) =
                &tree.arena.function(accessor).base.symbol.signature
            {
                accessors.insert(signature.clone());
            }
        }
    }
    (libraries, accessors)
}

/// The serialized IR of kotlinc-native 2.4.20's library is the ground truth: every accessor the
/// provider realizes is one the library's IR declares.
#[test]
fn fixture_properties_realize_the_accessors_their_ir_declares() {
    let (libraries, serialized) = fixture_libraries();
    let realized = |name: &str| {
        let property = single_property(&libraries, "fixture/signatures", name);
        let realization = property_realization(&libraries, &property);
        [Some(realization.getter), realization.setter]
            .into_iter()
            .flatten()
            .map(|identity| accessor_signature(&accessor_realization(&libraries, identity)).clone())
            .collect::<Vec<_>>()
    };

    let counter = realized("counter");
    assert_eq!(
        counter
            .iter()
            .map(KlibAccessorIdSignature::name)
            .collect::<Vec<_>>(),
        ["<get-counter>", "<set-counter>"]
    );
    let second = realized("second");
    assert_eq!(
        second
            .iter()
            .map(KlibAccessorIdSignature::name)
            .collect::<Vec<_>>(),
        ["<get-second>"]
    );
    let contextual = realized("contextual");
    for signature in counter.iter().chain(&second).chain(&contextual) {
        assert!(
            serialized.contains(signature),
            "{signature:?} is not declared by the fixture's IR"
        );
    }
}

#[test]
fn a_fixture_context_property_keeps_its_named_context_parameter() {
    let (libraries, _) = fixture_libraries();
    let contextual = single_property(&libraries, "fixture/signatures", "contextual");
    let text = ResolvedParameterIdentity::ContextValue {
        ordinal: 0,
        source_name: "text".into(),
    };
    assert_eq!(contextual.kind, PropKind::TopLevel);
    assert_eq!(contextual.context_count, 1);
    assert_eq!(contextual.context_param_names, ["text"]);
    assert_eq!(
        contextual.context_parameter_identities,
        std::slice::from_ref(&text)
    );
    assert_eq!(contextual.getter.params, vec![Ty::obj("kotlin/String")]);
    assert_eq!(contextual.getter.context_count, 1);
    let realization = property_realization(&libraries, &contextual);
    assert_eq!(
        *accessor_realization(&libraries, realization.getter).parameter_identities,
        [text]
    );
}

/// `companion val Registry.capacity` is named through `Registry`, not through its package.
#[test]
fn a_fixture_companion_extension_property_is_not_a_package_property() {
    let (libraries, _) = fixture_libraries();
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("fixture/signatures")),
        "capacity",
    );
    assert!(symbols.callables.properties().is_empty());
    assert!(symbols.callables.functions().is_empty());
}
