use super::super::module_slots::{ModuleSlotPlan, PlanKey, PlannedSlot};
use super::super::Representation;
use super::*;
use crate::fir::ExternalCallableId;

struct IntsAndReferences;

impl Representation for IntsAndReferences {
    fn same(&self, left: Ty, right: Ty) -> bool {
        left == right || (self.is_reference(left) && self.is_reference(right))
    }

    fn is_reference(&self, ty: Ty) -> bool {
        ty != Ty::Int && ty != Ty::Unit
    }

    fn owns_base(&self, _: TypeName) -> bool {
        false
    }
}

fn plan(members: Vec<ModuleMember>, forwarders: Vec<ModuleForwarder>) -> ModuleSlotPlan {
    let mut classes = HashMap::default();
    for (name, dependency, superclass, members, forwarders) in [
        ("Base", true, None, members, Vec::new()),
        (
            "Derived",
            false,
            Some(TypeName::from("Base")),
            Vec::new(),
            forwarders,
        ),
    ] {
        classes.insert(
            TypeName::from(name),
            ModuleClass {
                dependency,
                superclass,
                function_type_arities: Box::default(),
                members: members.into_boxed_slice(),
                function_overrides: Box::default(),
                property_overrides: Box::default(),
                forwarders: forwarders.into_boxed_slice(),
            },
        );
    }
    ModuleSlotPlan::build(
        &ModuleClasses {
            classes,
            interfaces: Vec::new(),
        },
        &IntsAndReferences,
    )
}

fn forwarder(parameters: Vec<Ty>, result: Ty, realizes: ForwardedMember) -> ModuleForwarder {
    ModuleForwarder {
        parameters: parameters.into_boxed_slice(),
        result,
        suspend: false,
        realizes: vec![realizes].into_boxed_slice(),
    }
}

fn function_plan(parameters: Vec<Ty>, result: Ty) -> ModuleSlotPlan {
    let target = ResolvedFunctionOverrideTarget::External(ExternalCallableId::from_raw(42));
    plan(
        vec![ModuleMember::Function {
            callable: target,
            parameters: vec![Ty::obj("kotlin/Any")].into_boxed_slice(),
            result: Ty::obj("kotlin/Any"),
            suspend: false,
        }],
        vec![forwarder(
            parameters,
            result,
            ForwardedMember::Function(target),
        )],
    )
}

#[test]
fn a_forwarder_reuses_the_exact_dependency_function_slot() {
    let plan = function_plan(vec![Ty::String], Ty::String);
    let target = ResolvedFunctionOverrideTarget::External(ExternalCallableId::from_raw(42));
    let base = plan.table(TypeName::from("Base")).unwrap().unwrap();
    let derived = plan.table(TypeName::from("Derived")).unwrap().unwrap();
    let inherited = base.slots[&PlanKey::function(target)];
    let key = PlanKey::Forwarder {
        class: TypeName::from("Derived"),
        ordinal: 0,
    };
    assert_eq!(derived.slots[&key], inherited);
    assert_eq!(
        derived.entries[inherited as usize],
        PlannedSlot::Member(key)
    );
    assert_eq!(derived.len(), base.len(), "the forwarder appends no slot");
}

#[test]
fn a_dependency_function_forwarder_cannot_change_parameter_or_result_carriers() {
    for (parameters, result) in [(vec![Ty::Int], Ty::String), (vec![Ty::String], Ty::Int)] {
        let plan = function_plan(parameters, result);
        assert!(plan
            .table(TypeName::from("Derived"))
            .unwrap()
            .unwrap_err()
            .contains("carried differently"));
    }
}

#[test]
fn a_forwarder_cannot_choose_between_distinct_inherited_class_slots() {
    let targets = [42, 43]
        .map(|raw| ResolvedFunctionOverrideTarget::External(ExternalCallableId::from_raw(raw)));
    let plan = plan(
        targets
            .iter()
            .map(|&callable| ModuleMember::Function {
                callable,
                parameters: Box::default(),
                result: Ty::String,
                suspend: false,
            })
            .collect(),
        vec![ModuleForwarder {
            parameters: Box::default(),
            result: Ty::String,
            suspend: false,
            realizes: targets.into_iter().map(ForwardedMember::Function).collect(),
        }],
    );
    assert!(plan
        .table(TypeName::from("Derived"))
        .unwrap()
        .unwrap_err()
        .contains("different slots"));
}

fn property_plan(receiver: Ty, getter: Ty, setter: Ty) -> ModuleSlotPlan {
    let target = ResolvedPropertyOverrideTarget::External(ExternalCallableId::from_raw(57));
    plan(
        vec![ModuleMember::Property {
            property: target,
            accessor_parameters: vec![Ty::String].into_boxed_slice(),
            value: Ty::obj("kotlin/Any"),
            getter_value: Ty::obj("kotlin/Any"),
            getter: true,
            setter: true,
        }],
        vec![
            forwarder(vec![receiver], getter, ForwardedMember::Getter(target)),
            forwarder(
                vec![receiver, setter],
                Ty::Unit,
                ForwardedMember::Setter(target),
            ),
        ],
    )
}

#[test]
fn property_forwarders_reuse_both_dependency_accessor_slots() {
    let plan = property_plan(Ty::String, Ty::String, Ty::String);
    let target = ResolvedPropertyOverrideTarget::External(ExternalCallableId::from_raw(57));
    let base = plan.table(TypeName::from("Base")).unwrap().unwrap();
    let derived = plan.table(TypeName::from("Derived")).unwrap().unwrap();
    for (ordinal, setter) in [(0, false), (1, true)] {
        let inherited = base.slots[&PlanKey::accessor(target, setter)];
        let key = PlanKey::Forwarder {
            class: TypeName::from("Derived"),
            ordinal,
        };
        assert_eq!(derived.slots[&key], inherited);
        assert_eq!(
            derived.entries[inherited as usize],
            PlannedSlot::Member(key)
        );
    }
    assert_eq!(derived.len(), base.len(), "neither accessor appends a slot");
}

#[test]
fn dependency_property_forwarders_check_receiver_getter_and_setter_carriers() {
    for (receiver, getter, setter) in [
        (Ty::Int, Ty::String, Ty::String),
        (Ty::String, Ty::Int, Ty::String),
        (Ty::String, Ty::String, Ty::Int),
    ] {
        let plan = property_plan(receiver, getter, setter);
        assert!(plan
            .table(TypeName::from("Derived"))
            .unwrap()
            .unwrap_err()
            .contains("carried differently"));
    }
}
