//! File-local classes must extend the producer's actual slots without publishing local identities.

use super::*;
use crate::backend::class_tables::fixtures::{
    add_method, class, function, record_semantic_override,
};
use crate::backend::class_tables::{build_numbered, LaidOutSuperclass, Slot, SlotKey};
use crate::fir::{ExternalCallableId, ResolvedPropertyOverrideTarget};
use crate::ir::{ClassId, FunId, IrFile, IrPropertyOverride};
use crate::types::{type_name, SemanticCallRole};

struct Carriers;

impl Representation for Carriers {
    fn same(&self, a: Ty, b: Ty) -> bool {
        self.is_reference(a) == self.is_reference(b)
    }
    fn is_reference(&self, ty: Ty) -> bool {
        ty != Ty::Int && ty != Ty::Unit
    }
    fn owns_base(&self, _: TypeName) -> bool {
        false
    }
}

fn producer_plan() -> ModuleSlotPlan {
    let key = PlanKey::DependencyCallable(ExternalCallableId::from_raw(2));
    let table = PlannedTable {
        superclass: None,
        inherited: 0,
        dependency: true,
        entries: vec![
            PlannedSlot::Any(AnyMember::Equals),
            PlannedSlot::Any(AnyMember::HashCode),
            PlannedSlot::Any(AnyMember::ToString),
            PlannedSlot::Reserved,
            PlannedSlot::Inherited { slot: 4 },
            PlannedSlot::Inherited { slot: 5 },
            PlannedSlot::Member(key),
        ],
        slots: [(key, 6)].into_iter().collect(),
    };
    ModuleSlotPlan {
        tables: [(type_name("producer/Base"), Ok(table))]
            .into_iter()
            .collect(),
        signatures: [(
            key,
            (vec![Ty::obj("kotlin/Any")].into_boxed_slice(), Ty::Unit),
        )]
        .into_iter()
        .collect(),
        interfaces: ModuleInterfaces::default(),
    }
}

fn local_member(overrides: bool) -> (IrFile, ClassId, FunId) {
    local_member_with_type(overrides, Ty::String)
}

fn local_member_with_type(overrides: bool, ty: Ty) -> (IrFile, ClassId, FunId) {
    let mut ir = IrFile::default();
    let local = class(&mut ir, "consumer/Local", "producer/Base", 1);
    ir.classes[local as usize].is_anonymous_object = true;
    let implementation = add_method(
        &mut ir,
        local,
        function("changed", "consumer/Local", vec![ty], Ty::Unit, false),
    );
    if overrides {
        record_semantic_override(
            &mut ir,
            local,
            implementation,
            SemanticCallRole::KotlinAnyToString,
        );
        let edge = &mut ir
            .function_overrides
            .get_mut(&type_name("consumer/Local"))
            .unwrap()[0];
        edge.implementation = edge.overridden;
        edge.overridden_owner = type_name("producer/Base");
        edge.overridden_semantic_role = None;
        edge.declared_parameters = vec![Ty::obj("kotlin/Any")];
        edge.applied_parameters = vec![Ty::String];
        edge.implementation_parameters = vec![ty];
    }
    (ir, local, implementation)
}

#[test]
fn a_file_local_representation_change_keeps_the_producer_frame_and_adds_its_own_slot() {
    let plan = producer_plan();
    let (ir, local, implementation) = local_member_with_type(true, Ty::Int);
    let tables = build_numbered(&Carriers, &ir, &plan, &|name| {
        (name == type_name("producer/Base")).then_some(LaidOutSuperclass {
            slots: 7,
            ..LaidOutSuperclass::default()
        })
    })
    .expect("a typed local bridge must preserve the producer's physical frame");
    assert_eq!(
        tables.table(local).vtable[6],
        Slot::Bridge {
            declared: super::super::BridgeDeclaration::Signature {
                parameters: vec![Ty::obj("kotlin/Any")].into_boxed_slice(),
                result: Ty::Unit,
            },
            target_slot: Some(7),
            target: implementation,
        }
    );
    assert_eq!(
        tables.table(local).vtable[7],
        Slot::Function(implementation)
    );
    assert_eq!(
        tables.slot(local, &SlotKey::Function(implementation)),
        Some(7)
    );
}

#[test]
fn a_file_local_override_replaces_the_exact_producer_slot() {
    let plan = producer_plan();
    let (ir, local, implementation) = local_member(true);
    let tables = build_numbered(&Carriers, &ir, &plan, &|name| {
        (name == type_name("producer/Base")).then_some(LaidOutSuperclass {
            slots: 7,
            ..LaidOutSuperclass::default()
        })
    })
    .expect("a local implementation must extend the authenticated producer table");
    assert_eq!(
        tables.table(local).vtable,
        vec![
            Slot::AnyMember(AnyMember::Equals),
            Slot::AnyMember(AnyMember::HashCode),
            Slot::AnyMember(AnyMember::ToString),
            Slot::Abstract,
            Slot::Inherited { slot: 4 },
            Slot::Inherited { slot: 5 },
            Slot::Function(implementation),
        ]
    );
    assert_eq!(
        tables.slot(local, &SlotKey::Function(implementation)),
        Some(6)
    );
    assert_eq!(
        tables.table(local).laid_out_superclass,
        Some(type_name("producer/Base"))
    );
    assert!(plan.table(type_name("consumer/Local")).is_none());
}

#[test]
fn the_same_spelling_without_an_override_keeps_the_producer_slot() {
    let plan = producer_plan();
    let (ir, local, implementation) = local_member(false);
    let tables = build_numbered(&Carriers, &ir, &plan, &|name| {
        (name == type_name("producer/Base")).then_some(LaidOutSuperclass {
            slots: 7,
            ..LaidOutSuperclass::default()
        })
    })
    .expect("a new local member follows the physical producer prefix");
    assert_eq!(tables.table(local).vtable[6], Slot::Inherited { slot: 6 });
    assert_eq!(
        tables.table(local).vtable[7],
        Slot::Function(implementation)
    );
    assert_eq!(
        tables.slot(local, &SlotKey::Function(implementation)),
        Some(7)
    );
    assert!(plan.table(type_name("consumer/Local")).is_none());
}

#[test]
fn file_property_accessors_keep_local_bodies_separate_from_the_property_identity() {
    let mut ir = IrFile::default();
    let local = class(&mut ir, "consumer/LocalProperty", "producer/Base", 1);
    ir.classes[local as usize].is_anonymous_object = true;
    let getter = add_method(
        &mut ir,
        local,
        function(
            "getValue",
            "consumer/LocalProperty",
            Vec::new(),
            Ty::String,
            false,
        ),
    );
    let setter = add_method(
        &mut ir,
        local,
        function(
            "setValue",
            "consumer/LocalProperty",
            vec![Ty::String],
            Ty::Unit,
            false,
        ),
    );
    let implementation =
        ResolvedPropertyOverrideTarget::Module(crate::fir::PropertyId::from_raw(9));
    let overridden = ResolvedPropertyOverrideTarget::External(ExternalCallableId::from_raw(11));
    ir.property_overrides.insert(
        type_name("consumer/LocalProperty"),
        vec![IrPropertyOverride {
            implementation,
            implementation_getter: Some(getter),
            implementation_setter: Some(setter),
            implementation_owner: type_name("consumer/LocalProperty"),
            overridden,
            overridden_owner: type_name("producer/Base"),
            overridden_is_interface: false,
            name: "value".into(),
            declared_type: Ty::obj("kotlin/Any"),
            applied_type: Ty::String,
            implementation_type: Ty::String,
            declared_receiver: None,
            implementation_receiver: None,
            overridden_setter_value: None,
            implementation_mutable: true,
            has_kotlin_superclass_override: true,
            depth: 1,
        }],
    );

    let projected = ModuleClasses::from_file_class(&ir, local)
        .expect("the authenticated property edge projects for this file only");
    let class = projected
        .class(type_name("consumer/LocalProperty"))
        .unwrap();
    assert!(matches!(
        &class.members[0],
        ModuleMember::Property {
            property: actual,
            getter_key: PlanKey::FileGetter {
                property: getter_property,
                function: getter_function,
            },
            setter_key: Some(PlanKey::FileSetter {
                property: setter_property,
                function: setter_function,
            }),
            ..
        } if *actual == implementation
            && *getter_property == implementation
            && *setter_property == implementation
            && *getter_function == getter
            && *setter_function == setter
    ));
    assert!(matches!(
        &class.property_overrides[0],
        ModulePropertyOverride {
            implementation: actual,
            getter_key: PlanKey::FileGetter { function, .. },
            setter_key: Some(PlanKey::FileSetter { function: set, .. }),
            overridden: target,
            ..
        } if *actual == implementation && *target == overridden
            && *function == getter && *set == setter
    ));
}

#[test]
fn file_property_receiver_shape_is_not_dropped_when_a_bridge_would_be_needed() {
    let getter = ExternalCallableId::from_raw(18);
    let key = PlanKey::DependencyGetter(getter);
    let table = PlannedTable {
        superclass: None,
        inherited: 0,
        dependency: true,
        entries: vec![
            PlannedSlot::Any(AnyMember::Equals),
            PlannedSlot::Any(AnyMember::HashCode),
            PlannedSlot::Any(AnyMember::ToString),
            PlannedSlot::Reserved,
            PlannedSlot::Inherited { slot: 4 },
            PlannedSlot::Inherited { slot: 5 },
            PlannedSlot::Member(key),
        ],
        slots: [(key, 6)].into_iter().collect(),
    };
    let plan = ModuleSlotPlan {
        tables: [(type_name("producer/ExtensionBase"), Ok(table))]
            .into_iter()
            .collect(),
        signatures: [(key, (vec![Ty::String].into_boxed_slice(), Ty::String))]
            .into_iter()
            .collect(),
        interfaces: ModuleInterfaces::default(),
    };
    let mut ir = IrFile::default();
    let local = class(
        &mut ir,
        "consumer/ExtensionProperty",
        "producer/ExtensionBase",
        1,
    );
    ir.classes[local as usize].is_anonymous_object = true;
    let implementation = crate::fir::PropertyId::from_raw(19);
    let method = add_method(
        &mut ir,
        local,
        function(
            "getValue",
            "consumer/ExtensionProperty",
            vec![Ty::Int],
            Ty::Int,
            false,
        ),
    );
    ir.property_overrides.insert(
        type_name("consumer/ExtensionProperty"),
        vec![IrPropertyOverride {
            implementation: ResolvedPropertyOverrideTarget::Module(implementation),
            implementation_getter: Some(method),
            implementation_setter: None,
            implementation_owner: type_name("consumer/ExtensionProperty"),
            overridden: ResolvedPropertyOverrideTarget::External(getter),
            overridden_owner: type_name("producer/ExtensionBase"),
            overridden_is_interface: false,
            name: "value".into(),
            declared_type: Ty::String,
            applied_type: Ty::String,
            implementation_type: Ty::Int,
            declared_receiver: Some(Ty::String),
            implementation_receiver: None,
            overridden_setter_value: None,
            implementation_mutable: false,
            has_kotlin_superclass_override: true,
            depth: 1,
        }],
    );

    let error = build_numbered(&Carriers, &ir, &plan, &|name| {
        (name == type_name("producer/ExtensionBase")).then_some(LaidOutSuperclass {
            slots: 7,
            ..LaidOutSuperclass::default()
        })
    })
    .expect_err("an unsupported member-extension bridge must be declined explicitly");
    assert!(error.contains("mismatched receiver roles"));
}
