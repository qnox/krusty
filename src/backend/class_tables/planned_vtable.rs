//! A class's dispatch table numbered by the module plan and filled from its declaring file's IR.
//!
//! [`super::module_slots`] decides which member sits at which number from the headers alone, so
//! every file of the module agrees. This file has the class's IR, and so it is the one that says
//! what each number holds: the method a member's callable lowered to, the field a default accessor
//! reads, or the bridge the plan placed. Anything the plan and the IR disagree about declines the
//! file, because a caller in another file is reading the plan's numbers.

use crate::fx_hash::{HashMap, HashSet};

use crate::fir::{CallableId, PropertyId, ResolvedPropertyOverrideTarget};
use crate::ir::{ClassId, FunId, IrClass, IrFile, IrLocalPropertyLayout};
use crate::types::TypeName;

use super::class_vtable::{
    interface_keys, register_inherited_interface_members, register_interface_keys,
    same_representation,
};
use super::members::{function_key, overridden_functions};
use super::module_interfaces::ModuleInterfaces;
use super::module_slots::{PlanKey, PlannedSlot, PlannedTable};
use super::{AnyMember, ClassTable, LaidOutSuperclass, Representation, Slot, SlotKey, Unsupported};

/// The class a planned table is filled for, and what it extends.
pub(super) struct PlannedClass<'t> {
    pub(super) id: ClassId,
    /// Its superclass in this file, already tabled as `parent`.
    pub(super) superclass: Option<ClassId>,
    pub(super) parent: Option<&'t ClassTable>,
    /// The nearest superclass another file laid out, which `superclass` (if any) extends in turn.
    pub(super) laid_out: Option<(TypeName, &'t LaidOutSuperclass)>,
}

/// The table of `planned_class` as `planned` numbers it.
///
/// A number below the slots of the superclass another file laid out, holding a member this file
/// does not lower, is that superclass's: [`Slot::Inherited`].
pub(super) fn planned_class_table(
    representation: &dyn Representation,
    ir: &IrFile,
    planned_class: PlannedClass<'_>,
    planned: &PlannedTable,
    numbered: &ModuleInterfaces,
) -> Result<ClassTable, Unsupported> {
    let PlannedClass {
        id,
        superclass,
        parent,
        laid_out,
    } = planned_class;
    let class = &ir.classes[id as usize];
    let qualified = class.fq_name();
    if let (None, Some((name, table))) = (parent, laid_out) {
        if table.slots != planned.inherited {
            return Err(format!(
                "a superclass of another file whose table the module plan does not number \
                 (`{qualified}` extends `{}`)",
                name.render().replace('/', ".")
            ));
        }
    }
    // Whether `slot` may hold what the superclass another file laid out holds there.
    let elsewhere = |slot: usize| laid_out.is_some_and(|(_, table)| (slot as u32) < table.slots);
    let lowered = |callable: CallableId| ir.checked_callable_functions.get(&callable).copied();
    let function = |callable: CallableId| {
        lowered(callable)
            .ok_or_else(|| format!("a planned member this file did not lower (`{qualified}`)"))
    };
    let method = |fid: FunId| {
        if ir.functions[fid as usize].body.is_some() {
            Slot::Function(fid)
        } else {
            Slot::Abstract
        }
    };
    let accessor = |property: PropertyId, setter: bool| -> Result<Slot, Unsupported> {
        match ir.local_property_layouts.get(&property) {
            Some(IrLocalPropertyLayout::Member {
                class,
                backing_field,
                getter,
                setter: setter_function,
                ..
            }) => Ok(
                match (
                    if setter { *setter_function } else { *getter },
                    backing_field,
                ) {
                    (Some(fid), _) => method(fid),
                    (None, Some(field)) if setter => Slot::FieldSetter {
                        class: *class,
                        field: *field,
                    },
                    (None, Some(field)) => Slot::FieldGetter {
                        class: *class,
                        field: *field,
                    },
                    (None, None) => Slot::Abstract,
                },
            ),
            Some(IrLocalPropertyLayout::MemberExtension {
                getter,
                setter: setter_function,
                ..
            }) => match if setter {
                *setter_function
            } else {
                Some(*getter)
            } {
                Some(fid) => Ok(method(fid)),
                None => Err(format!(
                    "a planned setter with no accessor in this file (`{qualified}`)"
                )),
            },
            _ => Err(format!(
                "a planned accessor of a property this file did not lower (`{qualified}`)"
            )),
        }
    };
    let mut vtable = Vec::with_capacity(planned.entries.len());
    for (slot, entry) in planned.entries.iter().enumerate() {
        vtable.push(match *entry {
            PlannedSlot::Any(member) => Slot::AnyMember(member),
            PlannedSlot::Reserved => Slot::Abstract,
            PlannedSlot::Inherited { slot } if elsewhere(slot as usize) => Slot::Inherited { slot },
            PlannedSlot::Inherited { .. } => {
                return Err("an opaque producer slot without its laid-out superclass".into())
            }
            PlannedSlot::Member(PlanKey::Callable(callable)) => match lowered(callable) {
                Some(fid) => method(fid),
                None if elsewhere(slot) => Slot::Inherited { slot: slot as u32 },
                None => method(function(callable)?),
            },
            PlannedSlot::Member(PlanKey::Getter(property) | PlanKey::Setter(property))
                if elsewhere(slot) && !ir.local_property_layouts.contains_key(&property) =>
            {
                Slot::Inherited { slot: slot as u32 }
            }
            PlannedSlot::Member(PlanKey::Forwarder { class, ordinal }) => {
                match forwarder(ir, class, ordinal) {
                    Some(fid) => method(fid),
                    None if elsewhere(slot) => Slot::Inherited { slot: slot as u32 },
                    None => {
                        return Err(format!(
                            "a planned interface-delegation forwarder this file did not lower \
                             (`{qualified}`)"
                        ))
                    }
                }
            }
            PlannedSlot::Member(PlanKey::Getter(property)) => accessor(property, false)?,
            PlannedSlot::Member(PlanKey::Setter(property)) => accessor(property, true)?,
            // A dependency class's member is a method of the support unit materializing the
            // class, and is inherited by a class extending it.
            PlannedSlot::Member(
                PlanKey::DependencyCallable(member) | PlanKey::DependencyGetter(member),
            ) => match ir.dependency_methods.get(&member) {
                Some(&fid) => method(fid),
                None if elsewhere(slot) => Slot::Inherited { slot: slot as u32 },
                None => {
                    return Err(format!(
                        "a planned dependency member this unit did not lower (`{qualified}`)"
                    ))
                }
            },
            PlannedSlot::Member(PlanKey::DependencySetter(_)) if elsewhere(slot) => {
                Slot::Inherited { slot: slot as u32 }
            }
            PlannedSlot::Member(PlanKey::DependencySetter(_)) => {
                return Err(format!(
                    "a planned setter of a dependency property (`{qualified}`)"
                ))
            }
            PlannedSlot::Bridge {
                declared,
                target_slot,
                target,
            } => match (lowered(declared), lowered(target)) {
                (None, None) if elsewhere(slot) => Slot::Inherited { slot: slot as u32 },
                (None, Some(_)) if elsewhere(slot) => {
                    return Err(format!(
                        "an override carried differently from the member of another file it \
                         overrides (`{qualified}`)"
                    ))
                }
                _ => Slot::Bridge {
                    declared: function(declared)?,
                    target_slot: Some(target_slot),
                    target: function(target)?,
                },
            },
            PlannedSlot::FunctionBridge {
                arity,
                target_slot,
                target,
            } => match lowered(target) {
                None if elsewhere(slot) => Slot::Inherited { slot: slot as u32 },
                _ => Slot::FunctionBridge {
                    arity,
                    target_slot,
                    target: function(target)?,
                },
            },
            PlannedSlot::AccessorBridge {
                declared,
                implemented,
                setter,
                target_slot,
            } => Slot::AccessorBridge {
                declared,
                implemented,
                receiver: None,
                setter,
                target_slot,
            },
        });
    }
    // Every member the IR dispatches has to be one the plan numbered; a method the headers do not
    // declare would sit at a number no other file knows about. The parent's table holds exactly
    // the numbers the plan gives it, so this one extends it.
    if parent.is_some_and(|parent| parent.vtable.len() as u32 != planned.inherited) {
        return Err(format!(
            "a superclass whose table differs from its module-wide plan (`{qualified}`)"
        ));
    }
    let mut slots: HashMap<SlotKey, u32> = parent
        .map(|parent| parent.slots.clone())
        .unwrap_or_default();
    let mut interface_bridges = parent
        .map(|parent| parent.interface_bridges.clone())
        .unwrap_or_default();
    slots.extend(AnyMember::ALL.map(|member| (SlotKey::Any(member.slot()), member.slot())));
    for (key, &slot) in &planned.slots {
        let key = match *key {
            // A member of the superclass another file laid out keeps its number in the plan,
            // which is where a caller reads it; this file has no function to key it by.
            PlanKey::Callable(callable) if laid_out.is_some() && lowered(callable).is_none() => {
                continue
            }
            PlanKey::Callable(callable) => SlotKey::Function(function(callable)?),
            PlanKey::Forwarder { class, ordinal } => match forwarder(ir, class, ordinal) {
                Some(fid) => SlotKey::Function(fid),
                // A forwarder of the superclass another file laid out.
                None => continue,
            },
            PlanKey::Getter(property) => {
                SlotKey::Getter(ResolvedPropertyOverrideTarget::Module(property))
            }
            PlanKey::Setter(property) => {
                SlotKey::Setter(ResolvedPropertyOverrideTarget::Module(property))
            }
            PlanKey::DependencyCallable(member) => match ir.dependency_methods.get(&member) {
                Some(&fid) => SlotKey::Function(fid),
                // As for a member of a superclass another file laid out.
                None => continue,
            },
            // A support unit emits a dependency property's dispatched accessor as a method.
            PlanKey::DependencyGetter(property) => match ir.dependency_methods.get(&property) {
                Some(&fid) => SlotKey::Function(fid),
                None => SlotKey::Getter(ResolvedPropertyOverrideTarget::External(property)),
            },
            PlanKey::DependencySetter(property) => {
                SlotKey::Setter(ResolvedPropertyOverrideTarget::External(property))
            }
        };
        slots.insert(key, slot);
    }
    let overridden = overridden_functions(ir, class, numbered, laid_out.is_some())?;
    let dispatched_dependency_members = ir
        .dependency_methods
        .values()
        .copied()
        .collect::<HashSet<_>>();
    let mut direct_methods = HashSet::default();
    for &fid in &class.methods {
        let function = &ir.functions[fid as usize];
        if function.dispatch_receiver.is_none() || function.is_static {
            continue;
        }
        let key = function_key(ir, id, fid);
        let slot = if ir.interface_delegation_forwarders.contains(&fid) {
            // The plan numbered every forwarder by its place among the class's forwarders.
            let slot = forwarder_ordinal(ir, class, fid)
                .and_then(|ordinal| {
                    planned.slots.get(&PlanKey::Forwarder {
                        class: class.fq_name,
                        ordinal,
                    })
                })
                .copied()
                .ok_or_else(|| {
                    format!(
                        "an interface-delegation forwarder the module plan does not number \
                         (`{qualified}.{}`)",
                        function.name
                    )
                })?;
            // One standing for an inherited class member must carry it alike.
            if let Some(replaced) =
                forwarded_class_slot(representation, ir, class, fid, &overridden, &slots)?
            {
                if replaced != slot {
                    return Err(format!(
                        "a delegated member the module plan numbers apart from the class member \
                         it overrides (`{qualified}.{}`)",
                        function.name
                    ));
                }
            }
            slots.insert(key, slot);
            slot
        } else {
            match slots.get(&key) {
                Some(&slot) => slot,
                // A support unit's non-dispatched dependency functions are called directly.
                None if planned.dependency && !dispatched_dependency_members.contains(&fid) => {
                    direct_methods.insert(fid);
                    continue;
                }
                None => {
                    return Err(format!(
                        "a member its declaration does not plan (`{qualified}.{}`)",
                        function.name
                    ))
                }
            }
        };
        let keys = interface_keys(representation, ir, function, &overridden, fid)?;
        register_interface_keys(class, fid, slot, keys, &mut slots, &mut interface_bridges);
    }
    register_inherited_interface_members(
        representation,
        ir,
        class,
        &mut slots,
        &mut interface_bridges,
    )?;

    Ok(ClassTable {
        superclass,
        laid_out_superclass: laid_out.map(|(name, _)| name),
        first_field: parent.map_or(0, |parent| {
            let parent_class = superclass.expect("a parent table is a superclass's");
            parent.first_field + ir.classes[parent_class as usize].fields.len() as u32
        }),
        vtable,
        slots,
        interface_bridges,
        direct_methods,
    })
}

/// The class number `forwarder`, a forwarder of `I by d`, replaces: the slot of an inherited class
/// member it overrides, or `None` when it implements interface members alone.
fn forwarded_class_slot(
    representation: &dyn Representation,
    ir: &IrFile,
    class: &IrClass,
    forwarder: FunId,
    overridden: &HashMap<FunId, Vec<FunId>>,
    slots: &HashMap<SlotKey, u32>,
) -> Result<Option<u32>, Unsupported> {
    let mismatch = || {
        format!(
            "a delegated member overriding a class member it carries differently (`{}.{}`)",
            class.fq_name(),
            ir.functions[forwarder as usize].name
        )
    };
    for &base in overridden.get(&forwarder).into_iter().flatten() {
        let function = &ir.functions[base as usize];
        let Some(owner) = function
            .dispatch_receiver
            .and_then(|owner| ir.class_id_by_name(owner))
            .filter(|&owner| !ir.classes[owner as usize].is_interface)
        else {
            continue;
        };
        if let Some(&slot) = slots.get(&function_key(ir, owner, base)) {
            if !same_representation(representation, &ir.functions[forwarder as usize], function) {
                return Err(mismatch());
            }
            return Ok(Some(slot));
        }
    }
    for edge in ir
        .property_overrides
        .get(&class.fq_name_id())
        .into_iter()
        .flatten()
        .filter(|edge| !edge.overridden_is_interface)
    {
        let key = if edge.implementation_getter == Some(forwarder) {
            SlotKey::Getter(edge.overridden)
        } else if edge.implementation_setter == Some(forwarder) {
            SlotKey::Setter(edge.overridden)
        } else {
            continue;
        };
        if let Some(&slot) = slots.get(&key) {
            if !representation.same(edge.implementation_type, edge.declared_type) {
                return Err(mismatch());
            }
            return Ok(Some(slot));
        }
    }
    Ok(None)
}

/// The `ordinal`-th forwarder the interface delegations of `class` generated in this file, `None`
/// when this file does not declare the class.
fn forwarder(ir: &IrFile, class: TypeName, ordinal: u32) -> Option<FunId> {
    let class = &ir.classes[ir.class_id_by_name(class)? as usize];
    forwarders(ir, class).nth(ordinal as usize)
}

/// Where `fid` stands among the forwarders of `class`.
fn forwarder_ordinal(ir: &IrFile, class: &IrClass, fid: FunId) -> Option<u32> {
    forwarders(ir, class)
        .position(|forwarder| forwarder == fid)
        .map(|ordinal| ordinal as u32)
}

/// The forwarders of `class`, in the order its delegations generated them.
fn forwarders<'i>(ir: &'i IrFile, class: &'i IrClass) -> impl Iterator<Item = FunId> + 'i {
    class
        .methods
        .iter()
        .copied()
        .filter(|fid| ir.interface_delegation_forwarders.contains(fid))
}
