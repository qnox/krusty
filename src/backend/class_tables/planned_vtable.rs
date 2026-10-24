//! A class's dispatch table numbered by the module plan and filled from its declaring file's IR.
//!
//! [`super::module_slots`] decides which member sits at which number from the headers alone, so
//! every file of the module agrees. This file has the class's IR, and so it is the one that says
//! what each number holds: the method a member's callable lowered to, the field a default accessor
//! reads, or the bridge the plan placed. Anything the plan and the IR disagree about declines the
//! file, because a caller in another file is reading the plan's numbers.

use crate::fx_hash::HashMap;

use crate::fir::{CallableId, PropertyId, ResolvedPropertyOverrideTarget};
use crate::ir::{ClassId, FunId, IrClass, IrFile, IrLocalPropertyLayout};

use super::class_vtable::{
    interface_keys, register_inherited_interface_members, register_interface_keys,
    same_representation,
};
use super::members::{function_key, overridden_functions};
use super::module_interfaces::ModuleInterfaces;
use super::module_slots::{PlanKey, PlannedSlot, PlannedTable};
use super::{AnyMember, ClassTable, Representation, Slot, SlotKey, Unsupported};

/// The table of class `id` as `planned` numbers it, extending `parent` (already tabled).
pub(super) fn planned_class_table(
    representation: &dyn Representation,
    ir: &IrFile,
    id: ClassId,
    superclass: Option<ClassId>,
    parent: Option<&ClassTable>,
    planned: &PlannedTable,
    numbered: &ModuleInterfaces,
) -> Result<ClassTable, Unsupported> {
    let class = &ir.classes[id as usize];
    let qualified = class.fq_name();
    let function = |callable: CallableId| {
        ir.checked_callable_functions
            .get(&callable)
            .copied()
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
    for entry in &planned.entries {
        vtable.push(match *entry {
            PlannedSlot::Any(member) => Slot::AnyMember(member),
            PlannedSlot::Reserved => Slot::Abstract,
            PlannedSlot::Member(PlanKey::Callable(callable)) => method(function(callable)?),
            PlannedSlot::Member(PlanKey::Getter(property)) => accessor(property, false)?,
            PlannedSlot::Member(PlanKey::Setter(property)) => accessor(property, true)?,
            PlannedSlot::Bridge {
                declared,
                target_slot,
                target,
            } => Slot::Bridge {
                declared: function(declared)?,
                target_slot: Some(target_slot),
                target: function(target)?,
            },
            PlannedSlot::FunctionBridge {
                arity,
                target_slot,
                target,
            } => Slot::FunctionBridge {
                arity,
                target_slot,
                target: function(target)?,
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
    // declare would sit at a number no other file knows about. A forwarder of `I by d` is the one
    // exception: it implements interface members alone, which no class number names, so it takes a
    // number past the planned ones, where no other file reads. A subclass gives the forwarders it
    // inherits numbers past its own planned ones in turn, so the parent's keys and bridges are
    // renumbered before this class's planned keys join them.
    let planned_len = vtable.len() as u32;
    let moved = |slot: &mut u32| {
        if *slot >= planned.inherited {
            *slot = *slot - planned.inherited + planned_len;
        }
    };
    let mut slots: HashMap<SlotKey, u32> = parent
        .map(|parent| parent.slots.clone())
        .unwrap_or_default();
    let mut interface_bridges = parent
        .map(|parent| parent.interface_bridges.clone())
        .unwrap_or_default();
    if let Some(parent) = parent {
        slots.values_mut().for_each(moved);
        for bridge in interface_bridges.values_mut() {
            match bridge {
                Slot::Bridge {
                    target_slot: Some(slot),
                    ..
                }
                | Slot::AccessorBridge {
                    target_slot: slot, ..
                } => moved(slot),
                _ => {}
            }
        }
        vtable.extend(parent.vtable[planned.inherited as usize..].iter().cloned());
    }
    slots.extend(AnyMember::ALL.map(|member| (SlotKey::Any(member.slot()), member.slot())));
    for (key, &slot) in &planned.slots {
        let key = match *key {
            PlanKey::Callable(callable) => SlotKey::Function(function(callable)?),
            PlanKey::Getter(property) => {
                SlotKey::Getter(ResolvedPropertyOverrideTarget::Module(property))
            }
            PlanKey::Setter(property) => {
                SlotKey::Setter(ResolvedPropertyOverrideTarget::Module(property))
            }
        };
        slots.insert(key, slot);
    }
    let overridden = overridden_functions(ir, class, numbered)?;
    for &fid in &class.methods {
        let function = &ir.functions[fid as usize];
        if function.dispatch_receiver.is_none() || function.is_static {
            continue;
        }
        let key = function_key(ir, id, fid);
        let slot = match slots.get(&key) {
            // A forwarder realizing a member this class inherits under the same key replaces
            // what the parent put there.
            Some(&slot) if ir.interface_delegation_forwarders.contains(&fid) => {
                vtable[slot as usize] = method(fid);
                slot
            }
            Some(&slot) => slot,
            None if ir.interface_delegation_forwarders.contains(&fid) => {
                // A forwarder that also stands for an inherited class member (`class E : B(), C by
                // D()` where both declare `x`) fills that member's number, as an override would.
                let slot = match forwarded_class_slot(
                    representation,
                    ir,
                    class,
                    fid,
                    &overridden,
                    &slots,
                )? {
                    Some(slot) => {
                        vtable[slot as usize] = method(fid);
                        slot
                    }
                    None => {
                        vtable.push(method(fid));
                        (vtable.len() - 1) as u32
                    }
                };
                slots.insert(key, slot);
                slot
            }
            None => {
                return Err(format!(
                    "a member its declaration does not plan (`{qualified}.{}`)",
                    function.name
                ))
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
        first_field: parent.map_or(0, |parent| {
            let parent_class = superclass.expect("a parent table is a superclass's");
            parent.first_field + ir.classes[parent_class as usize].fields.len() as u32
        }),
        vtable,
        slots,
        interface_bridges,
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
