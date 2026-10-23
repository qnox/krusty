//! A serialized fake-override reference is not a new provider declaration.

use super::super::serialized_references::{
    normalize_reference, KlibReferenceBinding, KlibReferenceDispatch, KlibReferenceOrigin,
};
use super::implementation_inventory::{
    declaration_ir_tree, declaration_ir_tree_kind, declaration_tree, fixture_class,
    fixture_property, identity, inventory, property_identity,
};
use super::*;
use crate::libraries::KlibDependencyCallable;
use crate::metadata::id_signature::{MetadataAccessor, MetadataClass};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrExprKind, KlibIrMember, KlibIrMemberAccess,
    KlibIrStatement, KlibIrType,
};
use crate::metadata::klib_ir::{
    KlibIrDeclarationTree, KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind,
};

fn member(owner: &str, function: &KotlinFunction, expect: bool) -> KlibDependencyCallable {
    identity(
        "declarations",
        &["fixture"],
        &[MetadataClass {
            name: owner,
            type_params: &[],
            expect,
        }],
        function,
    )
}

fn fixture(
    ambiguous: bool,
) -> (
    KlibLibraries,
    KlibDependencyCallable,
    KlibDependencyCallable,
) {
    fixture_with_edge(ambiguous, None)
}

fn fixture_with_edge(
    ambiguous: bool,
    edge: Option<(&str, KlibIrSymbolKind)>,
) -> (
    KlibLibraries,
    KlibDependencyCallable,
    KlibDependencyCallable,
) {
    let mut declaration = function("probe", None, Vec::new());
    declaration.ret = class("kotlin/Boolean", false);
    let canonical = member("Parent", &declaration, false);
    let reference = member("Child", &declaration, false);
    let mut child = fixture_class(Vec::new());
    child.supertypes = vec!["fixture/Parent".to_owned()];
    child.supertype_tys = vec![class("fixture/Parent", false)];
    let mut parent = fixture_class(vec![declaration]);
    parent.modality = crate::metadata::semantic::KotlinModality::Open;
    let package = KotlinPackage {
        classes: [
            ("fixture/Parent".to_owned(), parent),
            ("fixture/Child".to_owned(), child),
            ("fixture/Unrelated".to_owned(), fixture_class(Vec::new())),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let mut dependencies = vec!["declarations".to_owned()];
    let mut trees = vec![declaration_ir_tree(&canonical.declaration, "probe", None)];
    if let Some((edge, kind)) = edge {
        let mut function = declaration_ir_tree(&reference.declaration, "probe", None)
            .arena
            .function_at(0)
            .clone();
        function.base.origin = "FAKE_OVERRIDE".to_owned();
        let edge = member("Parent", &super::function(edge, None, Vec::new()), false);
        let KlibDeclarationSignature::Public(edge) = edge.declaration else {
            unreachable!()
        };
        function.overridden = vec![KlibIrSymbol {
            kind,
            signature: KlibIrSignature::Public(edge),
        }];
        let mut arena = KlibIrArena::default();
        function.return_type = arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        let function = arena.push_function(function);
        trees.push(KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Function(function),
        });
    }
    let mut bodies = vec![KlibBodyLibrary::new(
        "declarations".to_owned(),
        Vec::new(),
        KlibIrModuleTrees::from_trees(trees).unwrap(),
    )];
    if ambiguous {
        dependencies.push("other".to_owned());
        bodies.push(KlibBodyLibrary::new(
            "other".to_owned(),
            Vec::new(),
            declaration_tree(&canonical.declaration, "probe", None),
        ));
    }
    bodies.push(KlibBodyLibrary::new(
        "consumer".to_owned(),
        dependencies,
        KlibIrModuleTrees::default(),
    ));
    let libraries = KlibLibraries::over_with_bodies(
        Rc::new(inventory(vec![(
            segments(&["fixture"]),
            package,
            "declarations".to_owned(),
        )])),
        Rc::new(KlibDeclarationBodies::from_body_libraries(bodies).unwrap()),
    );
    (libraries, canonical, reference)
}

#[test]
fn an_explicit_fake_override_binds_only_through_its_serialized_override_edge() {
    let (libraries, canonical, reference) =
        fixture_with_edge(false, Some(("probe", KlibIrSymbolKind::Function)));
    let KlibReferenceOrigin::Exact(binding) =
        normalize_reference(&libraries, "consumer", &reference.declaration)
    else {
        panic!("the exact serialized override edge binds")
    };
    assert_eq!(binding.declaration, canonical);
    assert_eq!(
        binding.dispatch,
        Some(KlibReferenceDispatch {
            receiver: Ty::obj("fixture/Child"),
            declaring_receiver: Ty::obj("fixture/Parent")
        })
    );
    for edge in [
        ("different", KlibIrSymbolKind::Function),
        ("probe", KlibIrSymbolKind::Class),
    ] {
        let (libraries, _, reference) = fixture_with_edge(false, Some(edge));
        assert_eq!(
            normalize_reference(&libraries, "consumer", &reference.declaration),
            KlibReferenceOrigin::Missing
        );
    }
}

#[test]
fn an_inherited_reference_keeps_the_canonical_identity_and_its_dispatch_view() {
    let (libraries, canonical, reference) = fixture(false);
    assert_eq!(
        normalize_reference(&libraries, "consumer", &reference.declaration),
        KlibReferenceOrigin::Exact(KlibReferenceBinding {
            declaration: canonical,
            dispatch: Some(KlibReferenceDispatch {
                receiver: Ty::obj("fixture/Child"),
                declaring_receiver: Ty::obj("fixture/Parent"),
            }),
        })
    );
    assert!(!libraries
        .classifier(type_name("fixture/Child"))
        .unwrap()
        .declared_callables
        .contains_key("probe"));
}

#[test]
fn a_reference_does_not_select_between_two_reachable_producers() {
    let (libraries, _, reference) = fixture(true);
    assert_eq!(
        normalize_reference(&libraries, "consumer", &reference.declaration),
        KlibReferenceOrigin::Ambiguous
    );
}

#[test]
fn the_complete_projected_signature_not_the_member_hash_authenticates_a_reference() {
    let (libraries, _, reference) = fixture(false);
    let declaration = function("probe", None, Vec::new());
    let unrelated = member("Unrelated", &declaration, false);
    let KlibDeclarationSignature::Public(expected) = &reference.declaration else {
        unreachable!()
    };
    let KlibDeclarationSignature::Public(collision) = &unrelated.declaration else {
        unreachable!()
    };
    assert_eq!(expected.member_id(), collision.member_id());
    assert_ne!(expected, collision);
    for reference in [
        unrelated,
        member("Child", &declaration, true),
        member(
            "Child",
            &function("probe", None, vec![class("kotlin/Int", false)]),
            false,
        ),
    ] {
        assert_eq!(
            normalize_reference(&libraries, "consumer", &reference.declaration),
            KlibReferenceOrigin::Missing
        );
    }
}

#[test]
fn a_declaration_outside_the_body_producer_graph_grants_no_binding() {
    let (libraries, _, reference) = fixture(false);
    assert_eq!(
        normalize_reference(&libraries, "unrelated", &reference.declaration),
        KlibReferenceOrigin::Missing
    );
}

#[test]
fn freezing_keeps_constructor_and_accessor_references_as_exact_declarations() {
    let constructor = member("Owner", &function("<init>", None, Vec::new()), false);
    let property = fixture_property("value", false);
    let getter = property_identity(
        "declarations",
        &["fixture"],
        &[MetadataClass {
            name: "Owner",
            type_params: &[],
            expect: false,
        }],
        &property,
        MetadataAccessor::Getter,
    );
    let mut owner = fixture_class(Vec::new());
    owner
        .constructors
        .push(crate::metadata::semantic::KotlinConstructor {
            is_primary: true,
            params: Vec::new(),
            param_names: Vec::new(),
            param_defaults: Vec::new(),
            vararg: None,
            visibility: Visibility::Public,
        });
    owner.properties.push(property);
    let inventory = inventory(vec![(
        segments(&["fixture"]),
        KotlinPackage {
            classes: [("fixture/Owner".to_owned(), owner)].into_iter().collect(),
            ..Default::default()
        },
        "declarations".to_owned(),
    )]);
    let definitions = KlibIrModuleTrees::from_trees(vec![
        declaration_ir_tree_kind(&constructor.declaration, "<init>", true, None),
        declaration_ir_tree(&getter.declaration, "<get-value>", None),
    ])
    .unwrap();
    let bodies = KlibDeclarationBodies::from_body_libraries(vec![
        KlibBodyLibrary::new("declarations".to_owned(), Vec::new(), definitions),
        KlibBodyLibrary::new(
            "consumer".to_owned(),
            ["declarations".to_owned()],
            KlibIrModuleTrees::default(),
        ),
    ])
    .unwrap();

    // The body is only an identity-inventory fixture: no expression/header semantics execute.
    let caller = identity(
        "consumer",
        &["fixture"],
        &[],
        &function("caller", None, Vec::new()),
    );
    let mut function = declaration_ir_tree(&caller.declaration, "caller", None)
        .arena
        .function_at(0)
        .clone();
    let mut arena = KlibIrArena::default();
    let ty = arena.push_type(KlibIrType::Error {
        annotations: Vec::new(),
    });
    function.return_type = ty;
    let mut statements = Vec::new();
    for (declaration, constructor_call) in [
        (&constructor.declaration, true),
        (&getter.declaration, false),
    ] {
        let signature = match declaration {
            KlibDeclarationSignature::Public(signature) => {
                KlibIrSignature::Public(signature.clone())
            }
            KlibDeclarationSignature::Accessor(signature) => {
                KlibIrSignature::Accessor(signature.clone())
            }
        };
        let access = KlibIrMemberAccess {
            symbol: KlibIrSymbol {
                kind: if constructor_call {
                    KlibIrSymbolKind::Constructor
                } else {
                    KlibIrSymbolKind::Function
                },
                signature,
            },
            arguments: KlibIrArguments::Flat(Vec::new()),
            type_arguments: Vec::new(),
            origin: None,
        };
        let kind = if constructor_call {
            KlibIrExprKind::ConstructorCall {
                access,
                constructor_type_arguments: 0,
            }
        } else {
            KlibIrExprKind::Call {
                access,
                super_qualifier: None,
            }
        };
        let expression = arena.push_expr(Some(ty), kind);
        statements.push(KlibIrStatement::Expression(expression));
    }
    function.body = Some(KlibIrBody::Block(statements));
    let function = arena.push_function(function);
    let prepared = KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(function),
    }])
    .unwrap();
    let prepared = KlibDeclarationBodies::from_body_libraries(vec![KlibBodyLibrary::new(
        "consumer".to_owned(),
        ["declarations".to_owned()],
        prepared,
    )])
    .unwrap();
    let libraries = KlibLibraries::over_with_bodies_and_snapshot(
        Rc::new(inventory),
        Rc::new(bodies),
        Rc::new(prepared),
        None,
    );
    for declaration in [&constructor, &getter] {
        assert!(libraries.bodies.function(declaration).is_some());
        assert_eq!(
            libraries
                .reference_bindings()
                .get("consumer", &declaration.declaration),
            KlibReferenceOrigin::Exact(KlibReferenceBinding {
                declaration: declaration.clone(),
                dispatch: None
            })
        );
        let (_, realization) = libraries
            .external_callable_by_dependency(declaration)
            .expect("binding retains a real provider declaration, not only an archive index entry");
        assert_eq!(realization.dependency_callable.as_ref(), Some(declaration));
    }
    assert!(
        libraries
            .bodies
            .function(&constructor)
            .unwrap()
            .1
            .constructor
    );
    let fresh = libraries.for_compilation();
    assert!(
        Rc::ptr_eq(&libraries.references, &fresh.references),
        "normalization is not repeated per compilation"
    );
    assert_eq!(
        fresh
            .reference_bindings()
            .get("consumer", &getter.declaration),
        libraries
            .reference_bindings()
            .get("consumer", &getter.declaration)
    );
}

#[test]
// Platform-specific serialized archive shapes; Native and Wasm consume the same binding contract.
fn native_stdlib_fake_override_references_preserve_applied_parent_arguments() {
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE") else {
        assert!(std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none());
        return;
    };
    let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        std::path::PathBuf::from(root).join("klib/common/stdlib"),
        KlibStdlibSemantics::Native,
    )])
    .unwrap();
    for (package, receiver, name, parent) in [
        (
            &["kotlin", "collections"][..],
            "IntIterator",
            "hasNext",
            Ty::obj_args("kotlin/collections/Iterator", &[Ty::Int]),
        ),
        (
            &["kotlin"][..],
            "CharSequence",
            "toString",
            Ty::obj("kotlin/Any"),
        ),
    ] {
        let declaration = function(name, None, Vec::new());
        let signature = identity(
            "unused",
            package,
            &[MetadataClass {
                name: receiver,
                type_params: &[],
                expect: false,
            }],
            &declaration,
        );
        let owner = type_name(&format!("{}/{receiver}", package.join("/")));
        let module = libraries
            .inventory
            .classifier(owner)
            .unwrap()
            .module
            .as_deref()
            .unwrap();
        let KlibReferenceOrigin::Exact(binding) =
            normalize_reference(&libraries, module, &signature.declaration)
        else {
            panic!("the serialized {receiver}.{name} reference must bind")
        };
        let (_, canonical) = libraries
            .external_callable_by_dependency(&binding.declaration)
            .unwrap();
        assert_eq!(
            canonical.declaration_owner,
            Some(SemanticCallableOwner::Classifier(
                parent.obj_internal().unwrap()
            ))
        );
        assert_eq!(
            binding.dispatch,
            Some(KlibReferenceDispatch {
                receiver: Ty::obj_name(owner),
                declaring_receiver: parent
            })
        );
    }
}
