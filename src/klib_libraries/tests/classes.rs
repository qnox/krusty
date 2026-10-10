//! Classes published by the KLIB provider, with their constructors and members.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::klib::KlibArchive;
use crate::libraries::{
    ClassifierAccess, ExternalCallableRealization, LibraryMember, LibraryType, PropKind,
    PropertyInfo, TypeKind,
};
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrMember};
use crate::metadata::klib_ir::{read_declaration_trees, KlibIrSignature};
use crate::metadata::semantic::{
    parse_package_fragment_checked, KotlinClass, KotlinConstructor, KotlinModality,
};

const PACKAGE: &str = "fixture/signatures";

fn fixture_archive() -> KlibArchive {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/klib_signatures/signatures.klib");
    KlibArchive::open(&path).expect("the signature fixture KLIB opens")
}

fn fixture_packages(archive: &KlibArchive) -> Vec<(Vec<String>, KotlinPackage)> {
    archive
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
        .collect()
}

fn fixture() -> KlibLibraries {
    KlibLibraries::from_packages(fixture_packages(&fixture_archive()))
        .expect("the fixture is signable")
}

fn identity(local: &str) -> TypeName {
    type_name(&format!("{PACKAGE}/{local}"))
}

fn classifier(libraries: &KlibLibraries, local: &str) -> Arc<LibraryType> {
    libraries
        .classifier(identity(local))
        .unwrap_or_else(|| panic!("{local} is published"))
}

fn realization(
    libraries: &KlibLibraries,
    identity: Option<crate::fir::ExternalCallableId>,
) -> ExternalCallableRealization {
    libraries
        .external_callable(identity.expect("a published declaration carries its identity"))
        .expect("the provider answers for the identity it assigned")
}

fn public_path(realization: &ExternalCallableRealization) -> String {
    let Some(KlibDeclarationSignature::Public(signature)) = &realization.declaration_signature
    else {
        panic!("{} realizes a public signature", realization.callable.name);
    };
    signature.declaration().segments().join(".")
}

fn accessor_path(realization: &ExternalCallableRealization) -> String {
    let Some(KlibDeclarationSignature::Accessor(signature)) = &realization.declaration_signature
    else {
        panic!(
            "{} realizes an accessor signature",
            realization.callable.name
        );
    };
    format!(
        "{}.{}",
        signature.property().declaration().segments().join("."),
        signature.name()
    )
}

fn names(members: &[LibraryMember]) -> Vec<&str> {
    members.iter().map(|member| member.name.as_str()).collect()
}

fn declared_functions(shape: &LibraryType, name: &str) -> Vec<FunctionInfo> {
    shape
        .declared_callables
        .get(name)
        .map(|callables| callables.functions().to_vec())
        .unwrap_or_default()
}

fn single_function(shape: &LibraryType, name: &str) -> FunctionInfo {
    let [function] =
        declared_functions(shape, name)
            .try_into()
            .unwrap_or_else(|functions: Vec<_>| {
                panic!("expected one {name}, found {}", functions.len())
            });
    function
}

fn declared_property(shape: &LibraryType, name: &str) -> PropertyInfo {
    let callables = shape
        .declared_callables
        .get(name)
        .unwrap_or_else(|| panic!("{name} is declared"));
    let [property] = callables.properties() else {
        panic!("one property {name}");
    };
    property.clone()
}

#[test]
fn every_classifier_kind_is_published_with_its_exact_shape() {
    let libraries = fixture();
    let any = type_name("kotlin/Any");

    let shape = classifier(&libraries, "Shape");
    assert_eq!(shape.kind, TypeKind::Interface);
    assert!(shape.is_abstract());
    assert!(!shape.is_fun_interface());
    assert_eq!(shape.supertypes.iter_ids().collect::<Vec<_>>(), [any]);

    let action = classifier(&libraries, "Action");
    assert_eq!(action.kind, TypeKind::Interface);
    assert!(action.is_fun_interface());

    let marker = classifier(&libraries, "Marker");
    assert_eq!(marker.kind, TypeKind::Annotation);
    assert_eq!(marker.retention.as_deref(), Some("RUNTIME"));
    assert_eq!(
        marker.annotation_targets,
        Some(crate::types::AnnotationTargets::DEFAULT)
    );

    let singleton = classifier(&libraries, "Singleton");
    assert_eq!(singleton.kind, TypeKind::Object);
    assert!(singleton.constructors.is_empty());

    let color = classifier(&libraries, "Color");
    assert_eq!(color.kind, TypeKind::Enum);
    assert_eq!(color.enum_entries, ["RED", "GREEN"]);
    assert_eq!(
        color.supertype_templates,
        [Ty::obj_args(
            "kotlin/Enum",
            &[Ty::obj_name(identity("Color"))]
        )]
    );
    // An enum class's constructor is private: IR signs it, but no dependent can call it.
    assert!(color.constructors.is_empty());

    for (local, qualified) in [
        ("Shape", "fixture.signatures.Shape"),
        ("Outer.Inner", "fixture.signatures.Outer.Inner"),
    ] {
        let shape = classifier(&libraries, local);
        assert!(shape.is_kotlin);
        assert_eq!(shape.access, ClassifierAccess::Public);
        assert_eq!(shape.qualified_name.as_deref(), Some(qualified));
    }
}

#[test]
fn modality_and_direct_supertypes_come_from_metadata_alone() {
    let libraries = fixture();
    let base = classifier(&libraries, "Base");
    assert_eq!(base.kind, TypeKind::Class);
    assert!(base.is_abstract());
    assert!(!base.is_final());
    // Metadata lists a class's written supertypes; `Any` is implicit once another one is written.
    assert_eq!(
        base.supertypes.iter_ids().collect::<Vec<_>>(),
        [identity("Shape")]
    );

    let derived = classifier(&libraries, "Derived");
    assert!(!derived.is_abstract());
    assert!(!derived.is_final());
    // Direct supertypes only: `Shape` reaches `Derived` through `Base`, which core walks.
    assert_eq!(
        derived.supertype_templates,
        [
            Ty::obj_args_name(identity("Base"), &[Ty::String]),
            Ty::obj_args_name(
                type_name("kotlin/Comparable"),
                &[Ty::obj_name(identity("Derived"))]
            ),
        ]
    );
    assert_eq!(
        derived.companion_object,
        Some(("Factory".to_owned(), identity("Derived.Factory")))
    );
    assert!(!derived.inheritance.has_no_arg_constructor);

    let nested = classifier(&libraries, "Derived.Nested");
    assert!(nested.is_final());
    assert!(nested.is_nested);
    assert_eq!(nested.outer_instance, None);
    assert!(classifier(&libraries, "Derived.Factory").is_object());
}

#[test]
fn type_parameters_keep_their_bounds_variance_and_captured_scope() {
    let libraries = fixture();
    let base = classifier(&libraries, "Base");
    assert_eq!(
        base.type_params()
            .iter()
            .map(|parameter| crate::types::type_parameter_source_name(parameter))
            .collect::<Vec<_>>(),
        ["T"]
    );
    assert_eq!(
        *base.type_param_bounds(),
        [vec![Ty::obj("kotlin/CharSequence")]]
    );
    assert_eq!(*base.type_param_variances(), [TypeVariance::Invariant]);
    assert_eq!(base.own_type_parameter_count, 1);

    // An inner class captures its outer class's type parameters after its own.
    let outer = classifier(&libraries, "Outer");
    let inner = classifier(&libraries, "Outer.Inner");
    assert_eq!(
        inner
            .type_params()
            .iter()
            .map(|parameter| crate::types::type_parameter_source_name(parameter))
            .collect::<Vec<_>>(),
        ["U", "T"]
    );
    assert_ne!(
        inner.type_params()[0],
        inner.type_params()[1],
        "inner and captured parameters have declaration-owned identities"
    );
    assert_eq!(
        inner.type_params()[1],
        outer.type_params()[0],
        "the captured parameter retains the outer declaration's identity"
    );
    assert_eq!(inner.own_type_parameter_count, 1);
    assert_eq!(inner.outer_instance, Some(identity("Outer")));
    let both = single_function(&inner, "both");
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    assert_eq!(
        both.callable.params,
        [
            Ty::ty_param(&inner.type_params()[1], any),
            Ty::ty_param(&inner.type_params()[0], any)
        ]
    );

    // A member formal that is also written `T` shadows `Outer<T>` semantically, while a member
    // without a formal continues to refer to the class-owned identity.
    let shadow = single_function(&outer, "shadow");
    let shadow_sig = shadow.generic_sig.as_ref().expect("a generic member");
    assert_eq!(
        shadow_sig
            .formals
            .iter()
            .map(|formal| crate::types::type_parameter_source_name(formal))
            .collect::<Vec<_>>(),
        ["T"]
    );
    assert_ne!(shadow_sig.formals[0], outer.type_params()[0]);
    assert_eq!(
        shadow.callable.params,
        [Ty::ty_param(&shadow_sig.formals[0], any)]
    );
    let keep = single_function(&outer, "keep");
    assert_eq!(
        keep.callable.params,
        [Ty::ty_param(&outer.type_params()[0], any)]
    );
}

#[test]
fn a_nested_classifier_resolves_in_its_owner_namespace() {
    let libraries = fixture();
    let symbols = libraries.symbols(SymbolNamespace::Classifier(identity("Derived")), "Nested");
    assert_eq!(symbols.classifier_name, Some(identity("Derived.Nested")));
    assert_eq!(
        symbols.classifier_declaration,
        Some(crate::libraries::ClassifierDeclaration::Ordinary(identity(
            "Derived.Nested"
        )))
    );
    assert!(matches!(
        symbols.callables,
        crate::libraries::Callables::None
    ));
    // The package namespace holds only top-level classifiers.
    let package = SymbolNamespace::Package(type_name(PACKAGE));
    assert!(libraries.symbols(package, "Derived").classifier.is_some());
    assert!(libraries.symbols(package, "Nested").classifier.is_none());
}

#[test]
fn constructors_are_ordinary_callables_realizing_their_signatures() {
    let libraries = fixture();
    let derived = classifier(&libraries, "Derived");
    let shapes = derived
        .constructors
        .iter()
        .map(|constructor| {
            (
                constructor.is_primary_constructor(),
                constructor.params.clone(),
                constructor.call_sig.param_names.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        shapes,
        [
            (true, vec![Ty::String], vec!["label".to_owned()]),
            (false, vec![Ty::Int], vec!["count".to_owned()]),
        ]
    );
    let mut ids = HashSet::new();
    for constructor in &derived.constructors {
        let realized = realization(&libraries, constructor.external_identity);
        assert_eq!(realized.kind, ExternalCallableKind::Constructor);
        assert_eq!(
            realized.declaration_owner,
            Some(SemanticCallableOwner::Classifier(identity("Derived")))
        );
        assert_eq!(public_path(&realized), "Derived.<init>");
        assert_eq!(
            *realized.parameter_identities,
            constructor.call_sig.parameter_identities[..]
        );
        assert_eq!(realized.callable.compiler_intrinsic, None);
        assert_eq!(realized.callable.semantic_role, None);
        let Some(KlibDeclarationSignature::Public(signature)) = realized.declaration_signature
        else {
            unreachable!()
        };
        ids.insert(signature.member_id());
    }
    assert_eq!(ids.len(), 2);
    assert_eq!(
        derived
            .named_parameter_lists
            .iter()
            .map(|list| list.names.clone())
            .collect::<Vec<_>>(),
        [vec!["label".to_owned()], vec!["count".to_owned()]]
    );

    // `Outer` declares only a secondary vararg constructor; a generic class's constructor infers
    // the class's type argument.
    let outer = classifier(&libraries, "Outer");
    let [constructor] = outer.constructors.as_slice() else {
        panic!("one constructor");
    };
    assert!(!constructor.is_primary_constructor());
    assert_eq!(constructor.call_sig.vararg_index, Some(0));
    let generic = constructor.generic_sig.as_ref().expect("a generic class");
    assert_eq!(generic.formals, outer.type_params()[..1]);
    assert_eq!(
        generic.ret,
        Ty::obj_args_name(
            identity("Outer"),
            &[Ty::ty_param(
                &outer.type_params()[0],
                Ty::nullable(Ty::obj("kotlin/Any"))
            )]
        )
    );
}

#[test]
fn members_publish_their_kind_visibility_and_member_signature() {
    let libraries = fixture();
    let base = classifier(&libraries, "Base");
    let base_owner = Some(SemanticCallableOwner::Classifier(identity("Base")));
    // `private fun secret()` is signed in IR but is not visible to a dependent, so it is withheld.
    assert_eq!(
        base.declared_callable_order,
        ["hidden", "tuned", "scaled", "label"]
    );
    assert_eq!(names(&base.members), ["hidden", "tuned", "scaled"]);

    let hidden = single_function(&base, "hidden");
    assert_eq!(hidden.kind, FnKind::Member);
    assert_eq!(hidden.visibility, Visibility::Protected);
    assert!(hidden.flags.is_abstract);
    assert_eq!(hidden.callable.declaration_owner, base_owner);
    let realized = realization(&libraries, hidden.callable.external_identity);
    assert_eq!(realized.kind, ExternalCallableKind::Member);
    assert_eq!(public_path(&realized), "Base.hidden");
    assert_eq!(realized.callable.compiler_intrinsic, None);
    assert_eq!(realized.callable.semantic_role, None);

    let tuned = single_function(&base, "tuned");
    assert_eq!(tuned.visibility, Visibility::Internal);
    assert!(!tuned.flags.is_final);

    // `fun Int.scaled()` is a member extension: an extension for applicability, dispatched on
    // the class instance.
    let scaled = single_function(&base, "scaled");
    assert_eq!(scaled.kind, FnKind::Extension);
    assert_eq!(scaled.semantic_receiver(), Some(Ty::Int));
    assert!(base.members[2].is_member_extension());
    assert_eq!(
        base.members[2].external_identity,
        scaled.callable.external_identity
    );
    let realized = realization(&libraries, scaled.callable.external_identity);
    assert_eq!(realized.kind, ExternalCallableKind::Member);
    assert_eq!(
        *realized.parameter_identities,
        [ResolvedParameterIdentity::ExtensionReceiver]
    );

    let label = declared_property(&base, "label");
    assert_eq!(label.kind, PropKind::Member);
    assert_eq!(label.receiver, None);
    assert_eq!(
        label.ty,
        Ty::ty_param(&base.type_params()[0], Ty::obj("kotlin/CharSequence"))
    );
    let getter = realization(&libraries, label.getter.external_identity);
    assert_eq!(getter.kind, ExternalCallableKind::Member);
    assert_eq!(accessor_path(&getter), "Base.label.<get-label>");
    assert!(label.setter.is_none());

    let derived = classifier(&libraries, "Derived");
    let fetch = single_function(&derived, "fetch");
    assert!(fetch.flags.suspend);
    assert!(fetch.flags.is_final);
    let mutable = declared_property(&classifier(&libraries, "Outer"), "mutable");
    let setter = realization(
        &libraries,
        mutable.setter.as_ref().expect("a var").external_identity,
    );
    assert_eq!(accessor_path(&setter), "Outer.mutable.<set-mutable>");
}

#[test]
fn a_member_extension_property_is_a_member_extension() {
    let libraries = fixture();
    let extension = declared_property(&classifier(&libraries, "Outer"), "extension");
    assert_eq!(extension.kind, PropKind::MemberExtension);
    assert_eq!(
        extension
            .formals
            .iter()
            .map(|formal| crate::types::type_parameter_source_name(formal))
            .collect::<Vec<_>>(),
        ["R"]
    );
    assert_ne!(extension.formals, ["R"]);
    let getter = realization(&libraries, extension.getter.external_identity);
    assert_eq!(accessor_path(&getter), "Outer.extension.<get-extension>");
    assert_eq!(
        *getter.parameter_identities,
        [ResolvedParameterIdentity::ExtensionReceiver]
    );
}

#[test]
fn companion_block_members_and_companion_extensions_are_named_through_their_classifier() {
    let libraries = fixture();
    let registry = identity("Registry");
    let namespace = SymbolNamespace::Classifier(registry);
    let owner = Some(SemanticCallableOwner::Classifier(registry));

    let create = libraries.symbols(namespace, "create");
    let [create] = create.callables.functions() else {
        panic!("one create");
    };
    assert_eq!(create.kind, FnKind::TopLevel);
    assert_eq!(create.associated_classifier, Some(registry));
    assert_eq!(create.associated_access_owner, Some(registry));
    assert_eq!(create.callable.declaration_owner, owner);
    let realized = realization(&libraries, create.callable.external_identity);
    assert_eq!(realized.kind, ExternalCallableKind::TopLevel);
    assert_eq!(public_path(&realized), "Registry.create");
    let shape = classifier(&libraries, "Registry");
    assert_eq!(names(&shape.companion), ["create"]);
    assert_eq!(
        shape.companion[0].external_identity,
        create.callable.external_identity
    );
    assert!(shape.declared_callables.is_empty());

    let label = libraries.symbols(namespace, "label");
    let [label] = label.callables.properties() else {
        panic!("one label");
    };
    assert_eq!(label.kind, PropKind::TopLevel);
    assert_eq!(label.associated_classifier, Some(registry));
    let setter = realization(
        &libraries,
        label.setter.as_ref().expect("a var").external_identity,
    );
    assert_eq!(accessor_path(&setter), "Registry.label.<set-label>");

    // A companion extension is declared by its package and has no receiver parameter.
    let named = libraries.symbols(namespace, "named");
    let [named] = named.callables.functions() else {
        panic!("one named");
    };
    assert_eq!(named.kind, FnKind::TopLevel);
    assert_eq!(named.receiver, None);
    assert_eq!(named.callable.params, [Ty::String]);
    assert_eq!(named.associated_classifier, Some(registry));
    assert_eq!(named.associated_access_owner, None);
    let realized = realization(&libraries, named.callable.external_identity);
    assert_eq!(
        realized.declaration_owner,
        Some(SemanticCallableOwner::Package(type_name(PACKAGE)))
    );
    assert_eq!(public_path(&realized), "named");
    assert_eq!(
        *realized.parameter_identities,
        [ResolvedParameterIdentity::Source("name".into())]
    );

    let capacity = libraries.symbols(namespace, "capacity");
    let [capacity] = capacity.callables.properties() else {
        panic!("one capacity");
    };
    assert_eq!(capacity.receiver, None);
    let getter = realization(&libraries, capacity.getter.external_identity);
    assert!(getter.parameter_identities.is_empty());
    assert_eq!(accessor_path(&getter), "capacity.<get-capacity>");
}

#[test]
fn an_enum_class_realizes_its_implicit_members() {
    let libraries = fixture();
    let color = identity("Color");
    let shape = classifier(&libraries, "Color");
    assert_eq!(names(&shape.companion), ["values", "valueOf"]);
    let paths = shape
        .companion
        .iter()
        .map(|member| public_path(&realization(&libraries, member.external_identity)))
        .collect::<Vec<_>>();
    assert_eq!(paths, ["Color.values", "Color.valueOf"]);
    let value_of = realization(&libraries, shape.companion[1].external_identity);
    assert_eq!(
        *value_of.parameter_identities,
        [ResolvedParameterIdentity::Source("value".into())]
    );
    let entries = shape
        .enum_entries_accessor
        .as_ref()
        .expect("the enum was compiled with `entries`");
    assert_eq!(
        entries.ret,
        Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj_name(color)])
    );
    assert_eq!(
        accessor_path(&realization(&libraries, entries.external_identity)),
        "Color.entries.<get-entries>"
    );

    let values = libraries.symbols(SymbolNamespace::Classifier(color), "values");
    let [values] = values.callables.functions() else {
        panic!("one values");
    };
    assert_eq!(
        values.implicit_classifier_callable,
        Some(crate::libraries::ImplicitClassifierCallable::EnumValues)
    );
    assert_eq!(
        values.callable.external_identity,
        shape.companion[0].external_identity
    );
    // `classifier_callables` keeps the provider's realization for the implicit callables.
    assert_eq!(
        shape
            .classifier_callables(color)
            .iter()
            .map(|member| member.external_identity)
            .collect::<Vec<_>>(),
        [
            shape.companion[0].external_identity,
            shape.companion[1].external_identity
        ]
    );
}

/// The actual `Box` of the native fragment is the only `Box` metadata declares; the common
/// fragment's `expect class Box` is not serialized.
#[test]
fn an_actual_class_is_published_in_place_of_its_expectation() {
    let libraries = fixture();
    let shape = classifier(&libraries, "Box");
    assert_eq!(names(&shape.constructors), ["<init>"]);
    let open = single_function(&shape, "open");
    let realized = realization(&libraries, open.callable.external_identity);
    let Some(KlibDeclarationSignature::Public(signature)) = realized.declaration_signature else {
        unreachable!()
    };
    assert_eq!(signature.declaration().segments(), ["Box", "open"]);
    assert_eq!(signature.mask(), 0);
}

fn serialized_signatures(
    archive: &KlibArchive,
) -> (
    HashSet<KlibPublicIdSignature>,
    HashSet<KlibAccessorIdSignature>,
) {
    fn walk(
        arena: &KlibIrArena,
        member: &KlibIrMember,
        public: &mut HashSet<KlibPublicIdSignature>,
        accessors: &mut HashSet<KlibAccessorIdSignature>,
    ) {
        let mut record = |signature: &KlibIrSignature| match signature {
            KlibIrSignature::Public(signature) => {
                public.insert(signature.clone());
            }
            KlibIrSignature::Accessor(signature) => {
                accessors.insert(signature.clone());
            }
            KlibIrSignature::FileLocal { .. } => {}
        };
        match member {
            KlibIrMember::Function(function) => {
                record(&arena.function(*function).base.symbol.signature)
            }
            KlibIrMember::Property(property) => {
                record(&property.base.symbol.signature);
                for accessor in [property.getter, property.setter].into_iter().flatten() {
                    record(&arena.function(accessor).base.symbol.signature);
                }
            }
            KlibIrMember::Class(class) => {
                let class = arena.class(*class);
                record(&class.base.symbol.signature);
                for member in &class.members {
                    walk(arena, member, public, accessors);
                }
            }
            _ => {}
        }
    }
    let trees = read_declaration_trees(archive).expect("the fixture's IR decodes");
    let mut public = HashSet::new();
    let mut accessors = HashSet::new();
    for tree in trees.trees() {
        walk(&tree.arena, &tree.declaration, &mut public, &mut accessors);
    }
    (public, accessors)
}

/// Every identity a published class realizes, through any of its views.
fn realized_identities(
    libraries: &KlibLibraries,
    local: &str,
) -> Vec<crate::fir::ExternalCallableId> {
    let shape = classifier(libraries, local);
    let mut identities = Vec::new();
    for member in shape
        .constructors
        .iter()
        .chain(&shape.members)
        .chain(&shape.companion)
        .chain(&shape.enum_entries_accessor)
    {
        identities.extend(member.external_identity);
    }
    for callables in shape.declared_callables.values() {
        for function in callables.functions() {
            identities.extend(function.callable.external_identity);
        }
        for property in callables.properties() {
            identities.extend(property.getter.external_identity);
            identities.extend(
                property
                    .setter
                    .iter()
                    .flat_map(|setter| setter.external_identity),
            );
        }
    }
    identities
}

/// kotlinc-native 2.4.20's serialized IR is the ground truth: every class member, constructor,
/// and accessor the provider realizes is one the library's IR declares.
#[test]
fn fixture_classes_realize_the_signatures_their_ir_declares() {
    let archive = fixture_archive();
    let libraries =
        KlibLibraries::from_packages(fixture_packages(&archive)).expect("the fixture is signable");
    let (public, accessors) = serialized_signatures(&archive);
    let classes = [
        "Outer",
        "Outer.Inner",
        "Renamed",
        "Color",
        "Box",
        "Registry",
        "Shape",
        "Action",
        "Marker",
        "Base",
        "Derived",
        "Derived.Nested",
        "Derived.Factory",
        "Singleton",
    ];
    let mut realized = HashSet::new();
    for local in classes {
        realized.extend(realized_identities(&libraries, local));
    }
    let mut namespace_identities = Vec::new();
    for (owner, name) in [
        ("Registry", "create"),
        ("Registry", "size"),
        ("Registry", "label"),
        ("Registry", "named"),
        ("Registry", "capacity"),
        ("Color", "values"),
        ("Color", "valueOf"),
    ] {
        let symbols = libraries.symbols(SymbolNamespace::Classifier(identity(owner)), name);
        for function in symbols.callables.functions() {
            namespace_identities.extend(function.callable.external_identity);
        }
        for property in symbols.callables.properties() {
            namespace_identities.extend(property.getter.external_identity);
            namespace_identities.extend(
                property
                    .setter
                    .iter()
                    .flat_map(|setter| setter.external_identity),
            );
        }
    }
    assert_eq!(namespace_identities.len(), 8);
    realized.extend(namespace_identities);

    let mut paths = Vec::new();
    for identity in realized {
        let realization = libraries.external_callable(identity).expect("realization");
        match realization.declaration_signature.expect("a KLIB signature") {
            KlibDeclarationSignature::Public(signature) => {
                assert!(public.contains(&signature), "{signature:?} is not declared");
                paths.push(signature.declaration().segments().join("."));
            }
            KlibDeclarationSignature::Accessor(signature) => {
                assert!(
                    accessors.contains(&signature),
                    "{signature:?} is not declared"
                );
                paths.push(format!(
                    "{}.{}",
                    signature.property().declaration().segments().join("."),
                    signature.name()
                ));
            }
        }
    }
    paths.sort();
    assert_eq!(
        paths,
        [
            "Action.run",
            "Base.<init>",
            "Base.hidden",
            "Base.label.<get-label>",
            "Base.scaled",
            "Base.tuned",
            "Box.<init>",
            "Box.open",
            "Color.entries.<get-entries>",
            "Color.valueOf",
            "Color.values",
            "Derived.<init>",
            "Derived.<init>",
            "Derived.Factory.create",
            "Derived.Nested.<init>",
            "Derived.Nested.depth.<get-depth>",
            "Derived.area.<get-area>",
            "Derived.compareTo",
            "Derived.fetch",
            "Derived.hidden",
            "Marker.<init>",
            "Marker.level.<get-level>",
            "Outer.<init>",
            "Outer.Inner.<init>",
            "Outer.Inner.both",
            "Outer.extension.<get-extension>",
            "Outer.keep",
            "Outer.mutable.<get-mutable>",
            "Outer.mutable.<set-mutable>",
            "Outer.shadow",
            "Registry.<init>",
            "Registry.create",
            "Registry.label.<get-label>",
            "Registry.label.<set-label>",
            "Registry.size.<get-size>",
            "Renamed.<init>",
            "Renamed.keep",
            "Renamed.shadow",
            "Shape.area.<get-area>",
            "Shape.describe",
            "Singleton.LIMIT.<get-LIMIT>",
            "Singleton.ping",
            "capacity.<get-capacity>",
            "named",
        ]
    );
}

fn in_memory_class(constructor_params: usize) -> KotlinClass {
    KotlinClass {
        supertypes: vec!["kotlin/Any".to_owned()],
        supertype_tys: vec![class("kotlin/Any", false)],
        members: Vec::new(),
        functions: vec![function("run", None, vec![class("kotlin/Int", false)])],
        properties: Vec::new(),
        constructors: vec![KotlinConstructor {
            is_primary: true,
            params: vec![class("kotlin/Int", false); constructor_params],
            param_names: vec!["value".to_owned()],
            param_defaults: vec![false; constructor_params],
            vararg: None,
            visibility: Visibility::Public,
        }],
        companion_name: None,
        type_params: Vec::new(),
        kind: TypeKind::Class,
        is_fun_interface: false,
        visibility: Visibility::Public,
        is_expect: false,
        enum_entries: Vec::new(),
        has_enum_entries: false,
        sealed_subclasses: Vec::new(),
        inline_class_property: None,
        modality: KotlinModality::Final,
        is_nested: false,
        is_inner: false,
        metadata_flags: 0,
        annotations: Vec::new(),
        nullable_member_returns: Vec::new(),
    }
}

fn package_with_classes(classes: Vec<(&str, KotlinClass)>) -> KotlinPackage {
    KotlinPackage {
        classes: classes
            .into_iter()
            .map(|(name, class)| (name.to_owned(), class))
            .collect::<HashMap<_, _>>(),
        ..KotlinPackage::default()
    }
}

#[test]
fn a_class_repeated_across_fragments_publishes_one_classifier_and_one_identity_per_member() {
    let libraries = KlibLibraries::from_packages(vec![
        (
            segments(&["p"]),
            package_with_classes(vec![("p/Worker", in_memory_class(1))]),
        ),
        (
            segments(&["p"]),
            package_with_classes(vec![("p/Worker", in_memory_class(1))]),
        ),
    ])
    .expect("duplicate fragments are signable");
    let worker = libraries
        .classifier(type_name("p/Worker"))
        .expect("Worker is published");
    assert_eq!(worker.constructors.len(), 1);
    assert_eq!(declared_functions(&worker, "run").len(), 1);
    assert_eq!(worker.members.len(), 1);
    // The constructor and `run` hold the only two identities.
    assert!(libraries
        .external_callable(crate::fir::ExternalCallableId::from_raw(1))
        .is_some());
    assert!(libraries
        .external_callable(crate::fir::ExternalCallableId::from_raw(2))
        .is_none());
}

#[test]
fn a_member_keeps_its_contract_and_return_value_status_in_both_common_views() {
    let contract = Arc::new(crate::contracts::Contract {
        effects: vec![crate::contracts::Effect::Returns(
            crate::contracts::ReturnsValue::NotNull,
        )],
    });
    let mut worker = in_memory_class(1);
    worker.functions[0].ret = class("kotlin/String", true);
    worker.functions[0].contract = Some(contract.clone());
    worker.functions[0].return_value_status = crate::types::ReturnValueStatus::MustUse;
    let libraries = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_with_classes(vec![("p/Worker", worker)]),
    )])
    .expect("the class is signable");
    let worker = libraries
        .classifier(type_name("p/Worker"))
        .expect("Worker is published");

    let run = single_function(&worker, "run");
    assert_eq!(run.callable.contract.as_deref(), Some(contract.as_ref()));
    assert_eq!(
        run.flags.return_value_status,
        Some(crate::types::ReturnValueStatus::MustUse)
    );
    let [member] = worker.members.as_slice() else {
        panic!("one class-member projection");
    };
    assert_eq!(member.contract.as_deref(), Some(contract.as_ref()));
    assert_eq!(
        member.return_value_status,
        Some(crate::types::ReturnValueStatus::MustUse)
    );
}

#[test]
fn a_private_class_and_its_private_members_are_not_published() {
    let mut hidden = in_memory_class(1);
    hidden.visibility = Visibility::Private;
    let mut nested = in_memory_class(1);
    nested.is_nested = true;
    let mut open = in_memory_class(1);
    open.functions[0].visibility = Visibility::Private;
    open.constructors[0].visibility = Visibility::Private;
    let libraries = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_with_classes(vec![
            ("p/Hidden", hidden),
            ("p/Hidden.Nested", nested),
            ("p/Open", open),
        ]),
    )])
    .expect("signable classes");
    assert!(libraries.classifier(type_name("p/Hidden")).is_none());
    assert!(libraries.classifier(type_name("p/Hidden.Nested")).is_none());
    let open = libraries
        .classifier(type_name("p/Open"))
        .expect("Open is published");
    assert!(open.constructors.is_empty());
    assert!(open.members.is_empty());
    assert!(open.declared_callables.is_empty());
    assert!(!open.inheritance.has_no_arg_constructor);
}

#[test]
fn an_unsignable_member_rejects_the_library_set() {
    let mut malformed = in_memory_class(1);
    malformed.functions[0].context_count = 2;
    let error = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_with_classes(vec![("p/Worker", malformed)]),
    )])
    .err()
    .expect("a member without an identity cannot be published");
    assert_eq!(
        error.to_string(),
        "cannot sign KLIB declaration Worker.run in package `p`: \
         function run has 2 context parameters but 1 parameters"
    );

    let error = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_with_classes(vec![("p/Worker", in_memory_class(2))]),
    )])
    .err()
    .expect("a constructor without a name per parameter cannot be published");
    assert_eq!(
        error.to_string(),
        "cannot sign KLIB declaration Worker.<init> in package `p`: \
         constructor has 2 parameters but 1 parameter names"
    );

    let mut nested = in_memory_class(1);
    nested.is_nested = true;
    let error = KlibLibraries::from_packages(vec![(
        segments(&["p"]),
        package_with_classes(vec![("p/Outer.Nested", nested)]),
    )])
    .err()
    .expect("a nested class without its container cannot be signed");
    assert_eq!(
        error.to_string(),
        "cannot sign KLIB declaration Outer.Nested in package `p`: \
         enclosing class p/Outer is absent from its fragment"
    );
}
