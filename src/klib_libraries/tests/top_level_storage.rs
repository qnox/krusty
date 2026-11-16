//! Serialized facts a support unit relies on before it stores a top-level property itself: the
//! property an accessor belongs to, and whether the property's file initialization runs code.

use super::implementation_inventory::{fixture_property, property_identity};
use super::*;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::MetadataAccessor;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrDeclarationBase, KlibIrExprKind, KlibIrField, KlibIrFunction, KlibIrMember,
    KlibIrParameter, KlibIrProperty, KlibIrType,
};
use crate::metadata::klib_ir::{
    KlibIrConstant, KlibIrDeclarationTree, KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol,
    KlibIrSymbolKind,
};

const MODULE: &str = "storage";

fn base(kind: KlibIrSymbolKind, signature: KlibIrSignature) -> KlibIrDeclarationBase {
    KlibIrDeclarationBase {
        symbol: KlibIrSymbol { kind, signature },
        origin: "DEFINED".to_owned(),
        flags: 0,
        annotations: Vec::new(),
    }
}

fn accessor(name: &str, mutable: bool, which: MetadataAccessor) -> KlibDependencyCallable {
    property_identity(
        MODULE,
        &["fixture"],
        &[],
        &fixture_property(name, mutable),
        which,
    )
}

fn accessor_signature(identity: &KlibDependencyCallable) -> KlibIrSignature {
    let KlibDeclarationSignature::Accessor(signature) = &identity.declaration else {
        panic!("a fixture accessor has an accessor identity");
    };
    KlibIrSignature::Accessor(signature.clone())
}

fn property_signature(identity: &KlibDependencyCallable) -> KlibIrSignature {
    let KlibDeclarationSignature::Accessor(signature) = &identity.declaration else {
        panic!("a fixture accessor has an accessor identity");
    };
    KlibIrSignature::Public(signature.property().clone())
}

/// A function of `arena` declared as `signature`, with `parameters` value parameters.
fn function(
    arena: &mut KlibIrArena,
    signature: KlibIrSignature,
    parameters: usize,
) -> crate::metadata::klib_ir::tree::KlibIrFunctionId {
    let ty = arena.push_type(KlibIrType::Error {
        annotations: Vec::new(),
    });
    arena.push_function(KlibIrFunction {
        base: base(KlibIrSymbolKind::Function, signature),
        name: "<accessor>".to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: (0..parameters)
            .map(|slot| KlibIrParameter {
                base: base(
                    KlibIrSymbolKind::ValueParameter,
                    KlibIrSignature::FileLocal {
                        file: 0,
                        slot: u32::try_from(slot).expect("a fixture slot fits u32"),
                    },
                ),
                name: "<set-?>".to_owned(),
                ty,
                vararg_element_type: None,
                default_value: None,
            })
            .collect(),
        return_type: ty,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: None,
    })
}

/// A top-level property declared as `property`, whose getter and setter slots hold functions
/// declared as `getter` and `setter`.
fn property_tree(
    property: KlibIrSignature,
    getter: Option<(KlibIrSignature, usize)>,
    setter: Option<(KlibIrSignature, usize)>,
) -> KlibIrDeclarationTree {
    let mut arena = KlibIrArena::default();
    let getter = getter.map(|(signature, parameters)| function(&mut arena, signature, parameters));
    let setter = setter.map(|(signature, parameters)| function(&mut arena, signature, parameters));
    KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Property(KlibIrProperty {
            base: base(KlibIrSymbolKind::Property, property),
            name: "property".to_owned(),
            backing_field: None,
            getter,
            setter,
        }),
    }
}

/// A top-level property of serialized file `file` whose backing field (slot `slot`) is
/// initialized by `initializer`.
fn stored_property(
    name: &str,
    file: u32,
    slot: u32,
    initializer: Option<KlibIrExprKind>,
) -> KlibIrDeclarationTree {
    let identity = accessor(name, true, MetadataAccessor::Getter);
    let mut arena = KlibIrArena::default();
    let ty = arena.push_type(KlibIrType::Error {
        annotations: Vec::new(),
    });
    let initializer = initializer.map(|kind| arena.push_expr(Some(ty), kind));
    KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Property(KlibIrProperty {
            base: base(KlibIrSymbolKind::Property, property_signature(&identity)),
            name: name.to_owned(),
            backing_field: Some(KlibIrField {
                base: base(
                    KlibIrSymbolKind::Field,
                    KlibIrSignature::FileLocal { file, slot },
                ),
                name: name.to_owned(),
                ty,
                initializer,
            }),
            getter: None,
            setter: None,
        }),
    }
}

fn bodies(trees: Vec<KlibIrDeclarationTree>) -> KlibDeclarationBodies {
    KlibDeclarationBodies::from_libraries(vec![(
        MODULE.to_owned(),
        KlibIrModuleTrees::from_trees(trees).expect("the fixture trees have exact functions"),
    )])
    .expect("the fixture module has unique identities")
}

/// The name of the property `bodies` finds `identity` an accessor of, and whether it is the
/// setter.
fn owner(
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> Result<Option<(String, bool)>, &'static str> {
    bodies
        .top_level_accessor_property(identity)
        .map(|found| found.map(|(_, property, setter)| (property.name.clone(), setter)))
}

#[test]
fn an_accessor_belongs_only_to_the_property_its_signature_names() {
    let getter = accessor("counter", true, MetadataAccessor::Getter);
    let setter = accessor("counter", true, MetadataAccessor::Setter);
    let other = accessor("other", true, MetadataAccessor::Getter);
    // `other`'s property serializes `counter`'s accessors in its own slots.
    let misplaced = bodies(vec![property_tree(
        property_signature(&other),
        Some((accessor_signature(&getter), 0)),
        Some((accessor_signature(&setter), 1)),
    )]);
    assert_eq!(owner(&misplaced, &getter), Ok(None));
    assert_eq!(owner(&misplaced, &setter), Ok(None));

    let declared = bodies(vec![property_tree(
        property_signature(&getter),
        Some((accessor_signature(&getter), 0)),
        Some((accessor_signature(&setter), 1)),
    )]);
    assert_eq!(
        owner(&declared, &getter),
        Ok(Some(("property".to_owned(), false)))
    );
    assert_eq!(
        owner(&declared, &setter),
        Ok(Some(("property".to_owned(), true)))
    );

    // A slot holds only an accessor of its own shape: a getter without value parameters and a
    // setter with one.
    let swapped = bodies(vec![property_tree(
        property_signature(&getter),
        Some((accessor_signature(&setter), 1)),
        Some((accessor_signature(&getter), 0)),
    )]);
    assert_eq!(owner(&swapped, &getter), Ok(None));
    assert_eq!(owner(&swapped, &setter), Ok(None));
}

#[test]
fn a_file_initialization_is_inert_only_when_every_backing_field_is_constant() {
    let constant = || Some(KlibIrExprKind::Const(KlibIrConstant::Int(0)));
    let running = || Some(KlibIrExprKind::StringConcat(Vec::new()));
    let declarations = bodies(vec![
        stored_property("counter", 0, 1, constant()),
        stored_property("marker", 0, 2, running()),
        stored_property("limit", 1, 1, constant()),
        stored_property("pending", 1, 2, None),
    ]);
    assert!(!declarations.module_file_initialization_is_inert(MODULE, 0));
    assert!(declarations.module_file_initialization_is_inert(MODULE, 1));
    assert!(!declarations.module_file_initialization_is_inert("elsewhere", 1));
}
