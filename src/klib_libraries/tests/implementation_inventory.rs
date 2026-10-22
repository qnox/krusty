//! Exact declaration, body, and compiler-operation facts for KLIB implementation coverage.
//!
//! This deliberately stops before deciding which bodyful declaration requires compiler support.
//! Such a decision needs an authenticated semantic contract; an annotation spelling or a call
//! target's source name is not one. The inventory below keeps the provider-owned module plus
//! serialized declaration identity throughout and uses source rendering only in assertion output.

use std::rc::Rc;

use super::super::builtin_realizations;
use super::super::declaration_signatures::SignedFunction;
use super::super::inventory::{ArchivePackageInput, PackageInventory};
use super::super::lookup::{published_function, published_property};
use super::super::target_intrinsics::TargetIntrinsicPlatform;
use super::*;
use crate::fx_hash::HashMap;
use crate::libraries::{
    CallablePlacement, FunctionInfo, KlibDeclarationSignature, KlibDependencyCallable,
    LibraryCallable, MemberRealization, PropertyInfo, TypeKind,
};
use crate::metadata::id_signature::{
    constructor_signature, enum_class_member_signatures, metadata_class_signature,
    package_function_signature, package_property_accessor_signature, MetadataAccessor,
    MetadataClass, MetadataContainer,
};
use crate::metadata::klib_ir::tree::{
    KlibIrAnnotation, KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrDeclarationBase,
    KlibIrExprKind, KlibIrFunction, KlibIrMember, KlibIrMemberAccess, KlibIrNullability,
    KlibIrParameter, KlibIrType,
};
use crate::metadata::klib_ir::{
    KlibIrConstant, KlibIrDeclarationTree, KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol,
    KlibIrSymbolKind,
};
use crate::metadata::semantic::{
    KotlinClass, KotlinConstructor, KotlinFunction, KotlinModality, KotlinProperty,
    KotlinTypeParameter, KotlinTypeParameterId,
};
use crate::types::SemanticCallableOwner;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DecodedBodyState {
    /// No IR declaration with this exact module-qualified identity was decoded.
    Absent,
    /// The exact IR declaration exists but intentionally carries no executable body.
    Bodyless,
    /// The exact IR declaration carries a serialized body, including an empty block.
    Bodyful,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ImplementationFact {
    pub(super) placement: CallablePlacement,
    pub(super) body: DecodedBodyState,
    pub(super) external: bool,
    pub(super) has_operation: bool,
}

fn has_operation(callable: &LibraryCallable) -> bool {
    callable.compiler_intrinsic.is_some()
        || callable.semantic_role.is_some()
        || matches!(
            callable.member_realization,
            MemberRealization::Intrinsic(_) | MemberRealization::RangeConstruction { .. }
        )
}

fn normalized_function(
    libraries: &KlibLibraries,
    signed: &SignedFunction,
    placement: CallablePlacement,
    enclosing: &crate::libraries::EnclosingBounds,
) -> FunctionInfo {
    let mut function = published_function(&libraries.identities, signed, placement, enclosing);
    let realization = libraries
        .external_callable(
            function
                .callable
                .external_identity
                .expect("a provider-normalized function has an external identity"),
        )
        .expect("a provider-normalized function has a realization");
    if let Some(intrinsic) = realization
        .dependency_callable
        .as_ref()
        .and_then(|identity| libraries.inventory.intrinsic_facts().get(identity))
    {
        function.callable.compiler_intrinsic = Some(intrinsic.operation());
    }
    function
}

fn normalized_property(
    libraries: &KlibLibraries,
    signed: &super::super::declaration_signatures::SignedProperty,
    placement: CallablePlacement,
    enclosing: &crate::libraries::EnclosingBounds,
) -> PropertyInfo {
    let mut property = published_property(&libraries.identities, signed, placement, enclosing);
    if let CallablePlacement::Member { owner, .. } = placement {
        builtin_realizations::realize_member_property(owner, &mut property);
    }
    property
}

fn record_callable(
    facts: &mut HashMap<KlibDependencyCallable, ImplementationFact>,
    libraries: &KlibLibraries,
    callable: &LibraryCallable,
    placement: CallablePlacement,
    external: bool,
) {
    let realization = libraries
        .external_callable(
            callable
                .external_identity
                .expect("a provider-normalized callable has an external identity"),
        )
        .expect("a provider-normalized callable has a realization");
    let identity = realization
        .dependency_callable
        .clone()
        .expect("an archive declaration has an exact dependency identity");
    let body = match libraries.bodies.function_has_body(&identity) {
        None => DecodedBodyState::Absent,
        Some(false) => DecodedBodyState::Bodyless,
        Some(true) => DecodedBodyState::Bodyful,
    };
    let fact = ImplementationFact {
        placement,
        body,
        external,
        has_operation: realization.direct_c_function.is_some() || has_operation(callable),
    };
    assert_eq!(
        facts.insert(identity.clone(), fact),
        None,
        "duplicate exact implementation fact for {identity:?}"
    );
}

fn record_function(
    facts: &mut HashMap<KlibDependencyCallable, ImplementationFact>,
    libraries: &KlibLibraries,
    signed: &SignedFunction,
    placement: CallablePlacement,
    enclosing: &crate::libraries::EnclosingBounds,
) {
    let function = normalized_function(libraries, signed, placement, enclosing);
    record_callable(
        facts,
        libraries,
        &function.callable,
        placement,
        signed.declaration.is_external,
    );
}

fn record_property(
    facts: &mut HashMap<KlibDependencyCallable, ImplementationFact>,
    libraries: &KlibLibraries,
    signed: &super::super::declaration_signatures::SignedProperty,
    placement: CallablePlacement,
    enclosing: &crate::libraries::EnclosingBounds,
) {
    let property = normalized_property(libraries, signed, placement, enclosing);
    record_callable(facts, libraries, &property.getter, placement, false);
    if let Some(setter) = &property.setter {
        record_callable(facts, libraries, setter, placement, false);
    }
}

pub(super) fn implementation_inventory(
    libraries: &KlibLibraries,
) -> HashMap<KlibDependencyCallable, ImplementationFact> {
    let inventory = &libraries.inventory;
    let mut facts = HashMap::default();
    for (package, signed) in inventory.all_functions() {
        record_function(
            &mut facts,
            libraries,
            signed,
            CallablePlacement::Package(package),
            &Default::default(),
        );
    }
    for (package, signed) in inventory.all_properties() {
        record_property(
            &mut facts,
            libraries,
            signed,
            CallablePlacement::Package(package),
            &Default::default(),
        );
    }
    for (classifier, signed) in inventory.all_classifiers() {
        let member = CallablePlacement::Member {
            owner: classifier,
            owner_is_interface: signed.shape.is_interface(),
        };
        let enclosing = signed.type_parameters.enclosing();
        for function in &signed.functions {
            record_function(&mut facts, libraries, function, member, enclosing);
        }
        for property in &signed.properties {
            record_property(&mut facts, libraries, property, member, enclosing);
        }
        for constructor in &signed.constructors {
            let normalized = super::super::classifier_records::published_constructor(
                &libraries.identities,
                classifier,
                signed,
                constructor,
            );
            let realization = libraries
                .external_callable(
                    normalized
                        .external_identity
                        .expect("a provider-normalized constructor has an external identity"),
                )
                .expect("a provider-normalized constructor has a realization");
            record_callable(&mut facts, libraries, &realization.callable, member, false);
        }
        let associated = super::super::classifier_records::block_member_placement(classifier);
        for function in &signed.associated_functions {
            record_function(&mut facts, libraries, function, associated, enclosing);
        }
        for property in &signed.associated_properties {
            record_property(&mut facts, libraries, property, associated, enclosing);
        }
        for (_, normalized) in super::super::classifier_records::enum_class_members(
            &libraries.identities,
            classifier,
            signed,
        ) {
            let realization = libraries
                .external_callable(
                    normalized
                        .external_identity
                        .expect("a provider-normalized enum callable has an external identity"),
                )
                .expect("a provider-normalized enum callable has a realization");
            record_callable(
                &mut facts,
                libraries,
                &realization.callable,
                associated,
                false,
            );
        }
    }
    for (classifier, extension) in inventory.all_companion_functions() {
        record_function(
            &mut facts,
            libraries,
            &extension.signed,
            extension.placement(classifier),
            &Default::default(),
        );
    }
    for (classifier, extension) in inventory.all_companion_properties() {
        record_property(
            &mut facts,
            libraries,
            &extension.signed,
            extension.placement(classifier),
            &Default::default(),
        );
    }
    facts
}

pub(super) fn identity(
    module: &str,
    package: &[&str],
    classes: &[MetadataClass<'_>],
    function: &KotlinFunction,
) -> KlibDependencyCallable {
    identity_with_interop(module, package, classes, function, false)
}

pub(super) fn property_identity(
    module: &str,
    package: &[&str],
    classes: &[MetadataClass<'_>],
    property: &KotlinProperty,
    accessor: MetadataAccessor,
) -> KlibDependencyCallable {
    let package = segments(package);
    KlibDependencyCallable {
        module: module.to_owned(),
        declaration: KlibDeclarationSignature::Accessor(
            package_property_accessor_signature(
                MetadataContainer {
                    package: &package,
                    classes,
                    native_interop_library: false,
                },
                property,
                accessor,
            )
            .expect("the fixture property accessor is signable"),
        ),
    }
}

fn identity_with_interop(
    module: &str,
    package: &[&str],
    classes: &[MetadataClass<'_>],
    function: &KotlinFunction,
    native_interop_library: bool,
) -> KlibDependencyCallable {
    let package = segments(package);
    KlibDependencyCallable {
        module: module.to_owned(),
        declaration: KlibDeclarationSignature::Public(
            package_function_signature(
                MetadataContainer {
                    package: &package,
                    classes,
                    native_interop_library,
                },
                function,
            )
            .expect("the fixture declaration is signable"),
        ),
    }
}

pub(super) fn inventory(
    declarations: Vec<(
        Vec<String>,
        crate::metadata::semantic::KotlinPackage,
        String,
    )>,
) -> PackageInventory {
    PackageInventory::from_package_inputs(
        declarations
            .into_iter()
            .map(|(package, declarations, module)| {
                (package, declarations, false, false, Some(module))
            })
            .collect(),
    )
    .expect("the fixture declarations are signable")
}

pub(super) fn libraries(
    inventory: PackageInventory,
    bodies: Vec<(String, KlibIrModuleTrees)>,
) -> KlibLibraries {
    let bodies = super::super::KlibDeclarationBodies::from_libraries(bodies)
        .expect("the fixture body modules have unique exact identities");
    KlibLibraries::over_with_bodies(Rc::new(inventory), Rc::new(bodies))
}

fn declaration_tree(
    declaration: &KlibDeclarationSignature,
    name: &str,
    body: Option<KlibIrBody>,
) -> KlibIrModuleTrees {
    KlibIrModuleTrees::from_trees(vec![declaration_ir_tree(declaration, name, body)])
        .expect("the fixture declaration tree has one exact function")
}

fn declaration_ir_tree(
    declaration: &KlibDeclarationSignature,
    name: &str,
    body: Option<KlibIrBody>,
) -> KlibIrDeclarationTree {
    declaration_ir_tree_kind(declaration, name, false, body)
}

fn declaration_ir_tree_kind(
    declaration: &KlibDeclarationSignature,
    name: &str,
    constructor: bool,
    body: Option<KlibIrBody>,
) -> KlibIrDeclarationTree {
    let signature = match declaration {
        KlibDeclarationSignature::Public(signature) => KlibIrSignature::Public(signature.clone()),
        KlibDeclarationSignature::Accessor(signature) => {
            KlibIrSignature::Accessor(signature.clone())
        }
    };
    let mut arena = KlibIrArena::default();
    let return_type = arena.push_type(KlibIrType::Error {
        annotations: Vec::new(),
    });
    let function = arena.push_function(KlibIrFunction {
        base: KlibIrDeclarationBase {
            symbol: KlibIrSymbol {
                kind: if constructor {
                    KlibIrSymbolKind::Constructor
                } else {
                    KlibIrSymbolKind::Function
                },
                signature,
            },
            origin: "DEFINED".to_owned(),
            flags: 0,
            annotations: Vec::new(),
        },
        name: name.to_owned(),
        constructor,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: Vec::new(),
        return_type,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body,
    });
    KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(function),
    }
}

fn public_class_symbol(package: &[&str], name: &str) -> KlibIrSymbol {
    let package = segments(package);
    KlibIrSymbol {
        kind: KlibIrSymbolKind::Class,
        signature: KlibIrSignature::Public(metadata_class_signature(
            MetadataContainer {
                package: &package,
                classes: &[],
                native_interop_library: false,
            },
            MetadataClass {
                name,
                type_params: &[],
                expect: false,
            },
        )),
    }
}

fn local_base(kind: KlibIrSymbolKind, slot: u32) -> KlibIrDeclarationBase {
    KlibIrDeclarationBase {
        symbol: KlibIrSymbol {
            kind,
            signature: KlibIrSignature::FileLocal { file: 0, slot },
        },
        origin: "DEFINED".to_owned(),
        flags: 0,
        annotations: Vec::new(),
    }
}

pub(super) fn wasm_intrinsic_tree(callable: &KlibDependencyCallable) -> KlibIrModuleTrees {
    let KlibDeclarationSignature::Public(signature) = &callable.declaration else {
        panic!("a fixture function has a public identity")
    };
    let mut constructor = function("<init>", None, vec![class("kotlin/String", false)]);
    constructor.ret = class("kotlin/wasm/internal/WasmOp", false);
    let constructor = identity(
        "annotation-fixture",
        &["kotlin", "wasm", "internal"],
        &[MetadataClass {
            name: "WasmOp",
            type_params: &[],
            expect: false,
        }],
        &constructor,
    );
    let KlibDeclarationSignature::Public(constructor) = constructor.declaration else {
        panic!("an annotation constructor has a public identity")
    };

    let mut arena = KlibIrArena::default();
    let string = arena.push_expr(
        None,
        KlibIrExprKind::Const(KlibIrConstant::String("I32_REINTERPRET_F32".to_owned())),
    );
    let annotation = KlibIrAnnotation {
        access: KlibIrMemberAccess {
            symbol: KlibIrSymbol {
                kind: KlibIrSymbolKind::Constructor,
                signature: KlibIrSignature::Public(constructor),
            },
            arguments: KlibIrArguments::Flat(vec![Some(string)]),
            type_arguments: Vec::new(),
            origin: None,
        },
        constructor_type_arguments: 0,
    };
    let scalar = |classifier| KlibIrType::Simple {
        classifier,
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    };
    let float = arena.push_type(scalar(public_class_symbol(&["kotlin"], "Float")));
    let int = arena.push_type(scalar(public_class_symbol(&["kotlin"], "Int")));
    let parameter = KlibIrParameter {
        base: local_base(KlibIrSymbolKind::ValueParameter, 1),
        name: "value".to_owned(),
        ty: float,
        vararg_element_type: None,
        default_value: None,
    };
    let function = arena.push_function(KlibIrFunction {
        base: KlibIrDeclarationBase {
            symbol: KlibIrSymbol {
                kind: KlibIrSymbolKind::Function,
                signature: KlibIrSignature::Public(signature.clone()),
            },
            origin: "DEFINED".to_owned(),
            flags: 0,
            annotations: vec![annotation],
        },
        name: "wasm_i32_reinterpret_f32".to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: vec![parameter],
        return_type: int,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(Vec::new())),
    });
    KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(function),
    }])
    .expect("the fixture declaration tree has one exact function")
}

fn fact(
    placement: CallablePlacement,
    body: DecodedBodyState,
    external: bool,
    has_operation: bool,
) -> ImplementationFact {
    ImplementationFact {
        placement,
        body,
        external,
        has_operation,
    }
}

pub(super) fn fixture_property(name: &str, mutable: bool) -> KotlinProperty {
    KotlinProperty {
        name: name.to_owned(),
        receiver: None,
        context_params: Vec::new(),
        ty: class("kotlin/Int", false),
        formals: Vec::new(),
        visibility: Visibility::Public,
        modality: KotlinModality::Final,
        getter_modifiers: crate::metadata::semantic::KotlinAccessorModifiers::default_for(
            Visibility::Public,
            KotlinModality::Final,
        ),
        setter_modifiers: crate::metadata::semantic::KotlinAccessorModifiers::default_for(
            Visibility::Public,
            KotlinModality::Final,
        ),
        is_external: false,
        setter_parameter_name: None,
        is_var: mutable,
        is_const: false,
        is_expect: false,
        is_static: false,
        context_count: 0,
        context_param_names: Vec::new(),
        context_kinds: Vec::new(),
        constant: None,
        annotations: Vec::new(),
        return_value_status: Default::default(),
    }
}

#[test]
fn equal_declarations_in_distinct_modules_keep_distinct_inventory_identities() {
    let declaration = function("same", None, Vec::new());
    let first = identity("first", &["fixture"], &[], &declaration);
    let second = identity("second", &["fixture"], &[], &declaration);
    assert_eq!(first.declaration, second.declaration);

    let libraries = libraries(
        inventory(vec![
            (
                segments(&["fixture"]),
                package_of(vec![function("same", None, Vec::new())]),
                first.module.clone(),
            ),
            (
                segments(&["fixture"]),
                package_of(vec![function("same", None, Vec::new())]),
                second.module.clone(),
            ),
        ]),
        Vec::new(),
    );
    let actual = implementation_inventory(&libraries);
    let expected = HashMap::from_iter([
        (
            first,
            fact(
                CallablePlacement::Package(type_name("fixture")),
                DecodedBodyState::Absent,
                false,
                false,
            ),
        ),
        (
            second,
            fact(
                CallablePlacement::Package(type_name("fixture")),
                DecodedBodyState::Absent,
                false,
                false,
            ),
        ),
    ]);
    assert_eq!(actual, expected);
}

#[test]
fn decoded_body_state_distinguishes_absent_bodyless_and_bodyful_exactly() {
    let declaration = function("bodyState", None, Vec::new());
    let absent = identity("absent", &["fixture"], &[], &declaration);
    let bodyless = identity("bodyless", &["fixture"], &[], &declaration);
    let bodyful = identity("bodyful", &["fixture"], &[], &declaration);
    let package = || package_of(vec![function("bodyState", None, Vec::new())]);
    let libraries = libraries(
        inventory(vec![
            (segments(&["fixture"]), package(), absent.module.clone()),
            (segments(&["fixture"]), package(), bodyless.module.clone()),
            (segments(&["fixture"]), package(), bodyful.module.clone()),
        ]),
        vec![
            (absent.module.clone(), KlibIrModuleTrees::default()),
            (
                bodyless.module.clone(),
                declaration_tree(&bodyless.declaration, "bodyState", None),
            ),
            (
                bodyful.module.clone(),
                declaration_tree(
                    &bodyful.declaration,
                    "bodyState",
                    Some(KlibIrBody::Block(Vec::new())),
                ),
            ),
        ],
    );
    let actual = implementation_inventory(&libraries);
    let placement = CallablePlacement::Package(type_name("fixture"));
    assert_eq!(
        actual,
        HashMap::from_iter([
            (
                absent,
                fact(placement, DecodedBodyState::Absent, false, false),
            ),
            (
                bodyless,
                fact(placement, DecodedBodyState::Bodyless, false, false),
            ),
            (
                bodyful,
                fact(placement, DecodedBodyState::Bodyful, false, false),
            ),
        ])
    );
}

#[test]
fn property_accessors_keep_their_exact_identities_and_independent_body_states() {
    let module = "accessors";
    let package = ["fixture"];
    let property = fixture_property("counter", true);
    let getter = property_identity(module, &package, &[], &property, MetadataAccessor::Getter);
    let setter = property_identity(module, &package, &[], &property, MetadataAccessor::Setter);
    let trees = KlibIrModuleTrees::from_trees(vec![
        declaration_ir_tree(&getter.declaration, "<get-counter>", None),
        declaration_ir_tree(
            &setter.declaration,
            "<set-counter>",
            Some(KlibIrBody::Block(Vec::new())),
        ),
    ])
    .expect("the fixture accessors have distinct exact identities");
    let libraries = libraries(
        inventory(vec![(
            segments(&package),
            crate::metadata::semantic::KotlinPackage {
                properties: vec![property],
                ..Default::default()
            },
            module.to_owned(),
        )]),
        vec![(module.to_owned(), trees)],
    );
    let placement = CallablePlacement::Package(type_name("fixture"));

    assert_eq!(
        implementation_inventory(&libraries),
        HashMap::from_iter([
            (
                getter,
                fact(placement, DecodedBodyState::Bodyless, false, false),
            ),
            (
                setter,
                fact(placement, DecodedBodyState::Bodyful, false, false),
            ),
        ])
    );
}

#[test]
fn signed_constructors_and_implicit_enum_callables_are_inventory_entries() {
    let module = "classifier-callables";
    let package = ["fixture"];
    let constructor = KotlinConstructor {
        is_primary: true,
        params: vec![class("kotlin/Int", false)],
        param_names: vec!["value".to_owned()],
        param_defaults: vec![false],
        vararg: None,
        visibility: Visibility::Public,
    };
    let box_scope = [MetadataClass {
        name: "Box",
        type_params: &[],
        expect: false,
    }];
    let direction_scope = [MetadataClass {
        name: "Direction",
        type_params: &[],
        expect: false,
    }];
    let package_segments = segments(&package);
    let constructor_identity = KlibDependencyCallable {
        module: module.to_owned(),
        declaration: KlibDeclarationSignature::Public(
            constructor_signature(
                MetadataContainer {
                    package: &package_segments,
                    classes: &box_scope,
                    native_interop_library: false,
                },
                &constructor,
            )
            .expect("the fixture constructor is signable"),
        ),
    };
    let enum_signatures = enum_class_member_signatures(
        MetadataContainer {
            package: &package_segments,
            classes: &direction_scope,
            native_interop_library: false,
        },
        true,
    )
    .expect("the fixture enum callables are signable");
    let values = KlibDependencyCallable {
        module: module.to_owned(),
        declaration: KlibDeclarationSignature::Public(enum_signatures.values),
    };
    let value_of = KlibDependencyCallable {
        module: module.to_owned(),
        declaration: KlibDeclarationSignature::Public(enum_signatures.value_of),
    };
    let entries = KlibDependencyCallable {
        module: module.to_owned(),
        declaration: KlibDeclarationSignature::Accessor(
            enum_signatures
                .entries_getter
                .expect("the fixture enum declares entries"),
        ),
    };
    let trees = KlibIrModuleTrees::from_trees(vec![
        declaration_ir_tree_kind(&constructor_identity.declaration, "<init>", true, None),
        declaration_ir_tree(&values.declaration, "values", None),
        declaration_ir_tree(&value_of.declaration, "valueOf", None),
        declaration_ir_tree(&entries.declaration, "<get-entries>", None),
    ])
    .expect("the fixture classifier callables have distinct exact identities");
    let mut box_class = fixture_class(Vec::new());
    box_class.constructors = vec![constructor];
    let mut direction = fixture_class(Vec::new());
    direction.kind = TypeKind::Enum;
    direction.has_enum_entries = true;
    let libraries = libraries(
        inventory(vec![(
            package_segments,
            crate::metadata::semantic::KotlinPackage {
                classes: [
                    ("fixture/Box".to_owned(), box_class),
                    ("fixture/Direction".to_owned(), direction),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            },
            module.to_owned(),
        )]),
        vec![(module.to_owned(), trees)],
    );

    assert_eq!(
        implementation_inventory(&libraries),
        HashMap::from_iter([
            (
                constructor_identity,
                fact(
                    CallablePlacement::Member {
                        owner: type_name("fixture/Box"),
                        owner_is_interface: false,
                    },
                    DecodedBodyState::Bodyless,
                    false,
                    false,
                ),
            ),
            (
                values,
                fact(
                    CallablePlacement::Associated {
                        classifier: type_name("fixture/Direction"),
                        declaration_owner: SemanticCallableOwner::Classifier(type_name(
                            "fixture/Direction",
                        )),
                        access_owner: Some(type_name("fixture/Direction")),
                    },
                    DecodedBodyState::Bodyless,
                    false,
                    false,
                ),
            ),
            (
                value_of,
                fact(
                    CallablePlacement::Associated {
                        classifier: type_name("fixture/Direction"),
                        declaration_owner: SemanticCallableOwner::Classifier(type_name(
                            "fixture/Direction",
                        )),
                        access_owner: Some(type_name("fixture/Direction")),
                    },
                    DecodedBodyState::Bodyless,
                    false,
                    false,
                ),
            ),
            (
                entries,
                fact(
                    CallablePlacement::Associated {
                        classifier: type_name("fixture/Direction"),
                        declaration_owner: SemanticCallableOwner::Classifier(type_name(
                            "fixture/Direction",
                        )),
                        access_owner: Some(type_name("fixture/Direction")),
                    },
                    DecodedBodyState::Bodyless,
                    false,
                    false,
                ),
            ),
        ])
    );
}

#[test]
fn every_published_property_placement_contributes_each_exact_accessor_once() {
    let module = "property-placements";
    let package = ["fixture"];
    let worker = type_name("fixture/Worker");
    let worker_scope = [MetadataClass {
        name: "Worker",
        type_params: &[],
        expect: false,
    }];
    let top = fixture_property("top", true);
    let member = fixture_property("member", true);
    let mut associated = fixture_property("associated", true);
    associated.is_static = true;
    let mut companion = fixture_property("companion", true);
    companion.is_static = true;
    companion.receiver = Some(class("fixture/Worker", false));
    let identities = [
        (
            property_identity(module, &package, &[], &top, MetadataAccessor::Getter),
            property_identity(module, &package, &[], &top, MetadataAccessor::Setter),
            CallablePlacement::Package(type_name("fixture")),
        ),
        (
            property_identity(
                module,
                &package,
                &worker_scope,
                &member,
                MetadataAccessor::Getter,
            ),
            property_identity(
                module,
                &package,
                &worker_scope,
                &member,
                MetadataAccessor::Setter,
            ),
            CallablePlacement::Member {
                owner: worker,
                owner_is_interface: false,
            },
        ),
        (
            property_identity(
                module,
                &package,
                &worker_scope,
                &associated,
                MetadataAccessor::Getter,
            ),
            property_identity(
                module,
                &package,
                &worker_scope,
                &associated,
                MetadataAccessor::Setter,
            ),
            CallablePlacement::Associated {
                classifier: worker,
                declaration_owner: SemanticCallableOwner::Classifier(worker),
                access_owner: Some(worker),
            },
        ),
        (
            property_identity(module, &package, &[], &companion, MetadataAccessor::Getter),
            property_identity(module, &package, &[], &companion, MetadataAccessor::Setter),
            CallablePlacement::Associated {
                classifier: worker,
                declaration_owner: SemanticCallableOwner::Package(type_name("fixture")),
                access_owner: None,
            },
        ),
    ];
    let mut worker_class = fixture_class(Vec::new());
    worker_class.properties = vec![member, associated];
    let libraries = libraries(
        inventory(vec![(
            segments(&package),
            crate::metadata::semantic::KotlinPackage {
                classes: [("fixture/Worker".to_owned(), worker_class)]
                    .into_iter()
                    .collect(),
                properties: vec![top, companion],
                ..Default::default()
            },
            module.to_owned(),
        )]),
        Vec::new(),
    );
    let mut actual = implementation_inventory(&libraries);
    assert_eq!(actual.len(), identities.len() * 2);
    for (getter, setter, placement) in identities {
        assert_eq!(
            actual.remove(&getter),
            Some(fact(placement, DecodedBodyState::Absent, false, false,))
        );
        assert_eq!(
            actual.remove(&setter),
            Some(fact(placement, DecodedBodyState::Absent, false, false,))
        );
    }
    assert_eq!(actual, HashMap::default());
}

#[test]
fn an_interop_direct_c_declaration_is_an_exact_bodyless_operation() {
    let module = "direct-c";
    let package = ["fixture", "directc"];
    let mut declaration = function(
        "krusty_fixture_sum",
        None,
        vec![class("kotlin/Int", false), class("kotlin/Int", false)],
    );
    declaration.ret = class("kotlin/Int", false);
    declaration.is_external = true;
    let annotation = crate::metadata::semantic::AnnotationApplication {
        identity: type_name("kotlinx/cinterop/internal/CCall$Direct"),
        arguments: vec![(
            "name".to_owned(),
            crate::metadata::semantic::AnnotationArgument::String("krusty_fixture_sum".to_owned()),
        )],
    };
    declaration.annotations = vec![annotation.identity];
    declaration.annotation_applications = vec![annotation];
    let exact = identity_with_interop(module, &package, &[], &declaration, true);
    let inventory = PackageInventory::from_target_package_inputs(vec![ArchivePackageInput {
        segments: segments(&package),
        package: package_of(vec![declaration]),
        compiler_builtin_owner: false,
        internal_friend: false,
        module: Some(module.to_owned()),
        intrinsic_platform: None,
        native_interop_library: true,
    }])
    .expect("the exact interop declaration is signable");
    let libraries = libraries(inventory, Vec::new());

    assert_eq!(
        implementation_inventory(&libraries),
        HashMap::from_iter([(
            exact,
            fact(
                CallablePlacement::Package(type_name("fixture/directc")),
                DecodedBodyState::Absent,
                true,
                true,
            ),
        )])
    );
}

fn fixture_class(functions: Vec<KotlinFunction>) -> KotlinClass {
    KotlinClass {
        supertypes: vec!["kotlin/Any".to_owned()],
        supertype_tys: vec![class("kotlin/Any", false)],
        members: Vec::new(),
        functions,
        properties: Vec::new(),
        type_aliases: Vec::new(),
        constructors: Vec::new(),
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

#[test]
fn prepared_inline_filter_keeps_an_associated_member_signature() {
    let module = "prepared-associated";
    let mut associated = function("materialize", None, Vec::new());
    associated.is_static = true;
    associated.is_inline = true;
    associated.has_reified_type_params = true;
    associated.formals = vec![KotlinTypeParameter {
        id: KotlinTypeParameterId(1),
        name: "T".to_owned(),
        bounds: vec![class("kotlin/Any", true)],
        variance: crate::types::TypeVariance::Invariant,
        only_input: false,
        reified: true,
    }];
    let inventory = inventory(vec![(
        segments(&["fixture"]),
        crate::metadata::semantic::KotlinPackage {
            classes: [("fixture/Owner".to_owned(), fixture_class(vec![associated]))]
                .into_iter()
                .collect(),
            ..Default::default()
        },
        module.to_owned(),
    )]);
    let classifier = inventory
        .classifier(type_name("fixture/Owner"))
        .expect("associated member owner is published");
    let [associated] = classifier.associated_functions.as_slice() else {
        panic!("one associated member must be signed")
    };
    let signature = KlibIrSignature::Public(associated.signature.clone());

    assert!(inventory
        .inline_body_selection()
        .contains(module, &signature));
}

#[test]
fn every_callable_placement_has_one_exact_inventory_fact() {
    let module = "placements";
    let fixture_package = ["fixture"];
    let worker = type_name("fixture/Worker");
    let worker_scope = [MetadataClass {
        name: "Worker",
        type_params: &[],
        expect: false,
    }];
    let int = type_name("kotlin/Int");
    let int_scope = [MetadataClass {
        name: "Int",
        type_params: &[],
        expect: false,
    }];
    let top = println();
    let mut member = function("plus", None, vec![class("kotlin/Int", false)]);
    member.ret = class("kotlin/Int", false);
    member.is_operator = true;
    let mut block = function("block", None, Vec::new());
    block.is_static = true;
    let mut extension = function(
        "extension",
        Some(class("fixture/Worker", false)),
        Vec::new(),
    );
    extension.is_static = true;
    let top_identity = identity(module, &["kotlin", "io"], &[], &top);
    let member_identity = identity(module, &["kotlin"], &int_scope, &member);
    let block_identity = identity(module, &fixture_package, &worker_scope, &block);
    let extension_identity = identity(module, &fixture_package, &[], &extension);

    let libraries = libraries(
        inventory(vec![
            (
                segments(&["kotlin", "io"]),
                package_of(vec![top]),
                module.to_owned(),
            ),
            (
                segments(&["kotlin"]),
                crate::metadata::semantic::KotlinPackage {
                    classes: [("kotlin/Int".to_owned(), fixture_class(vec![member]))]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
                module.to_owned(),
            ),
            (
                segments(&fixture_package),
                crate::metadata::semantic::KotlinPackage {
                    classes: [("fixture/Worker".to_owned(), fixture_class(vec![block]))]
                        .into_iter()
                        .collect(),
                    functions: vec![extension],
                    ..Default::default()
                },
                module.to_owned(),
            ),
        ]),
        Vec::new(),
    );
    let actual = implementation_inventory(&libraries);
    assert_eq!(
        actual,
        HashMap::from_iter([
            (
                top_identity,
                fact(
                    CallablePlacement::Package(type_name("kotlin/io")),
                    DecodedBodyState::Absent,
                    false,
                    true,
                ),
            ),
            (
                member_identity,
                fact(
                    CallablePlacement::Member {
                        owner: int,
                        owner_is_interface: false,
                    },
                    DecodedBodyState::Absent,
                    false,
                    true,
                ),
            ),
            (
                block_identity,
                // Current compiler-operation providers authenticate package and member
                // declarations only. Associated functions still belong in the exact inventory,
                // but this fixture must not manufacture an operation for one.
                fact(
                    CallablePlacement::Associated {
                        classifier: worker,
                        declaration_owner: SemanticCallableOwner::Classifier(worker),
                        access_owner: Some(worker),
                    },
                    DecodedBodyState::Absent,
                    false,
                    false,
                ),
            ),
            (
                extension_identity,
                // Companion extensions likewise have no compiler-owned realization today; this
                // assertion covers their placement and the absence of an invented operation.
                fact(
                    CallablePlacement::Associated {
                        classifier: worker,
                        declaration_owner: SemanticCallableOwner::Package(type_name("fixture")),
                        access_owner: None,
                    },
                    DecodedBodyState::Absent,
                    false,
                    false,
                ),
            ),
        ])
    );
}

#[test]
fn an_operation_covers_only_the_exact_module_qualified_declaration() {
    let declaration = || {
        let mut function = function(
            "wasm_i32_reinterpret_f32",
            None,
            vec![class("kotlin/Float", false)],
        );
        function.ret = class("kotlin/Int", false);
        function
    };
    let implemented = identity(
        "implemented",
        &["kotlin", "wasm", "internal"],
        &[],
        &declaration(),
    );
    let unimplemented = identity(
        "unimplemented",
        &["kotlin", "wasm", "internal"],
        &[],
        &declaration(),
    );
    assert_eq!(implemented.declaration, unimplemented.declaration);

    let implemented_trees = wasm_intrinsic_tree(&implemented);
    let unimplemented_trees = wasm_intrinsic_tree(&unimplemented);
    let mut inventory = inventory(vec![
        (
            segments(&["kotlin", "wasm", "internal"]),
            package_of(vec![declaration()]),
            implemented.module.clone(),
        ),
        (
            segments(&["kotlin", "wasm", "internal"]),
            package_of(vec![declaration()]),
            unimplemented.module.clone(),
        ),
    ]);
    inventory.attach_ir_intrinsics(
        &implemented.module,
        Some(TargetIntrinsicPlatform::Wasm),
        &implemented_trees,
    );
    inventory.attach_ir_intrinsics(&unimplemented.module, None, &unimplemented_trees);
    assert!(inventory.intrinsic_facts().get(&implemented).is_some());
    assert!(inventory.intrinsic_facts().get(&unimplemented).is_none());

    let libraries = libraries(
        inventory,
        vec![
            (implemented.module.clone(), implemented_trees),
            (unimplemented.module.clone(), unimplemented_trees),
        ],
    );
    let placement = CallablePlacement::Package(type_name("kotlin/wasm/internal"));
    assert_eq!(
        implementation_inventory(&libraries),
        HashMap::from_iter([
            (
                implemented,
                fact(placement, DecodedBodyState::Bodyful, false, true),
            ),
            (
                unimplemented,
                fact(placement, DecodedBodyState::Bodyful, false, false),
            ),
        ])
    );
}
