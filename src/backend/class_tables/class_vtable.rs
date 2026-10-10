//! One class's dispatch table: its superclass's, with overridden slots replaced and newly declared
//! members appended.

use std::collections::HashMap;

use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{ClassId, FunId, IrClass, IrFile, IrFunction};
use crate::types::Ty;

use super::members::{
    any_slot, function_key, implements_an_interface, local_property_target, overridden_functions,
    overridden_properties, overridden_property_slot, property_accessor_key, property_type,
};
use super::{any_vtable, ClassTable, Representation, Slot, SlotKey, Unsupported, FUNCTION_SLOT};

/// Build the table of class `id`, whose superclass in this file (already tabled) is `parent`.
pub(super) fn class_table(
    representation: &dyn Representation,
    ir: &IrFile,
    id: ClassId,
    superclass: Option<ClassId>,
    parent: Option<&ClassTable>,
) -> Result<ClassTable, Unsupported> {
    let class = &ir.classes[id as usize];
    // Bridges against an INTERFACE's member numbers, which are placed program-wide rather than in
    // this table. Filled below wherever an implementation and the interface's declaration disagree
    // about representation, and read where the interface region is laid out.
    // Inherited, because a subclass's vtable inherits the slot the bridge forwards through and
    // would otherwise take the raw implementation at the interface's number.
    let mut interface_bridges: HashMap<SlotKey, Slot> = parent
        .map(|parent| parent.interface_bridges.clone())
        .unwrap_or_default();
    // A root class starts from `kotlin.Any`'s table. What fills those three slots for a base the
    // target's runtime owns — a `Throwable` renders as `qualified.Name: message` rather than by
    // identity — is the target's to say; here they are `kotlin.Any`'s members, by role.
    let (mut vtable, mut slots) = match parent {
        Some(parent) => (parent.vtable.clone(), parent.slots.clone()),
        None => any_vtable(),
    };
    // The FUNCTION SLOT follows `kotlin.Any`'s three in every table, whether or not the class is a
    // function. A class that becomes one lower in the hierarchy — `class B : A(), () -> String` —
    // inherits its superclass's table, and a member of `A` already standing at that number would be
    // overwritten by `invoke` while every call through `A` still expected it there. Reserved at the
    // root, the number is never anyone else's.
    if vtable.len() <= FUNCTION_SLOT as usize {
        vtable.resize(FUNCTION_SLOT as usize + 1, Slot::Abstract);
    }

    // A class implementing MORE THAN ONE function type has more `invoke`s than there are fixed
    // numbers to put them in. `KT_SLOT_INVOKE` is one number, and `object Test : () -> Unit,
    // (Boolean) -> Unit` declares two bodies that both belong there — whichever took it, a call
    // through the other function type would reach the wrong one (`intersectionTypeToFun`
    // `InterfaceConversion.kt` answered `KK` for `OK`). Decline rather than pick.
    {
        let invokes = ir
            .function_overrides
            .get(&class.fq_name_id())
            .into_iter()
            .flatten()
            .filter(|edge| {
                edge.overridden_semantic_role
                    == Some(crate::types::SemanticCallRole::KotlinFunctionInvoke)
                    && matches!(edge.overridden, ResolvedFunctionOverrideTarget::External(_))
            })
            .map(|edge| edge.overridden_owner)
            .collect::<std::collections::HashSet<_>>();
        if invokes.len() > 1 {
            return Err(format!(
                "a class implementing {} function types at once (`{}`)",
                invokes.len(),
                class.fq_name()
            ));
        }
    }

    // A class naming a function type of more than 22 parameters among its supertypes. Kotlin
    // declares `invoke` on `Function0` through `Function22` only, so a wider arity has no declaration
    // for an override edge to reach: nothing says which method is the function's body, the function
    // slot stays abstract, and every call through the type would reach it. Decline rather than pick
    // a method by its name.
    if let Some(arity) = class
        .supertypes
        .iter()
        .filter_map(|supertype| match supertype.non_null() {
            Ty::Fun(signature) => Some(signature.params.len()),
            _ => None,
        })
        .find(|&arity| arity > 22)
    {
        return Err(format!(
            "a class implementing a function type of {arity} parameters (`{}`)",
            class.fq_name()
        ));
    }

    // Which methods are property accessors, so their slots are keyed by the property. Common
    // lowering records bodyless abstract accessors too, so this boundary consumes exact function
    // identities and never reconstructs a property relationship from an accessor spelling.
    //
    // Only where the property has no STORAGE of its own. A property with a field is read through
    // that field — the accessor for it is synthesized below — so a method that happens to spell
    // the accessor's name is a method and not that accessor. `class Bottom(val data: Int) : Top {
    // override fun getData(): Int = data }` declares both, which is legal Kotlin and not even
    // unusual; keying the method as the property's getter took it out of the method numbering
    // entirely, so the base's slot kept the base's body and a call through the base jumped into
    // whatever stood there.
    let accessor_keys: HashMap<FunId, SlotKey> = class
        .methods
        .iter()
        .filter_map(|&function| property_accessor_key(ir, id, function).map(|key| (function, key)))
        .collect();
    let overridden_functions = overridden_functions(ir, class)?;
    let overridden_properties = overridden_properties(ir, id, class)?;

    for &fid in &class.methods {
        let function = &ir.functions[fid as usize];
        if function.dispatch_receiver.is_none() || function.is_static {
            continue;
        }
        let entry = if function.body.is_some() {
            Slot::Function(fid)
        } else {
            Slot::Abstract
        };
        let own_key = accessor_keys
            .get(&fid)
            .cloned()
            .unwrap_or(SlotKey::Function(fid));

        let mut override_edges = ir
            .function_overrides
            .get(&class.fq_name_id())
            .into_iter()
            .flatten()
            .filter(|edge| {
                // Either shape of implementation reference. An edge for a method the checked
                // lowering built names it by CALLABLE and leaves `implementation_function` empty,
                // which a pre-filter on that field alone used to drop — and dropping it is why
                // `invoke` never took the slot by this route at all.
                edge.implementation_function == Some(fid)
                    || ir
                        .checked_callable_functions
                        .get(match &edge.implementation {
                            ResolvedFunctionOverrideTarget::Module(callable) => callable,
                            ResolvedFunctionOverrideTarget::External(_) => return false,
                        })
                        == Some(&fid)
            });
        let generated_any_replaces = [
            (crate::ir::IrDataClassMemberRole::Equals, 0),
            (crate::ir::IrDataClassMemberRole::HashCode, 1),
            (crate::ir::IrDataClassMemberRole::ToString, 2),
        ]
        .into_iter()
        .find_map(|(role, slot)| {
            (ir.data_class_member(class.fq_name_id(), role) == Some(fid)).then_some(slot)
        });
        let override_any_replaces = override_edges
            .clone()
            .find_map(|edge| any_slot(edge.overridden_semantic_role));
        if let (Some(generated), Some(overridden)) = (generated_any_replaces, override_any_replaces)
        {
            assert_eq!(
                generated, overridden,
                "a synthesized data-class role and its override edge must name the same Any slot"
            );
        }
        let any_replaces = generated_any_replaces.or(override_any_replaces);
        // The edge by which this method overrides a function classifier's `invoke`, if it does.
        // Read once, because both the fixed slot below and the stand-in that covers a representation
        // mismatch are the same question about the same exact edge.
        let invoke_edge = override_edges.find(|edge| {
            edge.overridden_semantic_role
                == Some(crate::types::SemanticCallRole::KotlinFunctionInvoke)
        });
        let replaces = match any_replaces {
            Some(slot) => Some(slot),
            // `invoke` on a class implementing a FUNCTION TYPE takes the one other fixed slot this
            // target has: the runtime names it (`KT_SLOT_INVOKE`) and every caller through a
            // function type reads it, a lambda's body included.
            None => {
                invoke_edge.and_then(|edge| external_invoke_slot(representation, edge, function))
            }
        };
        // Whether the FUNCTION SLOT needs a stand-in for this method: it is an `invoke` over a
        // function type that could not take the slot outright, because it does not carry
        // references throughout. `replaces` is `None` for it, so the method takes a slot of its
        // own below and the fixed number gets the converting entry once that slot is known.
        let function_bridge = match (replaces, invoke_edge) {
            (None, Some(_)) => Some(function.params.len()),
            _ => None,
        };
        // What this method overrides, split by what each target owns: a class base owns a slot in
        // this vtable to replace, while an interface base owns a number in the program-wide
        // interface region, which is pointed at this method's slot once that slot is known.
        // The interface bases this method satisfies, each with the declaration a BRIDGE against
        // that number would wear — `None` where the representations already agree.
        let mut interface_keys: Vec<(SlotKey, Option<FunId>)> = Vec::new();
        let mut class_replaces = None;
        // A base whose signature has a different REPRESENTATION keeps its own slot and gets a
        // bridge placed in it once this method's slot is known. A class base can always take one;
        // an interface base's number is program-wide rather than this vtable's, so one there is
        // still declined.
        let mut bridged: Vec<(u32, FunId)> = Vec::new();
        // The same, for a property accessor whose base declares a different representation. It is
        // kept apart because neither end need be a source accessor, so the entry carries the two
        // property TYPES rather than two declaration ids.
        let mut accessor_bridged: Vec<(u32, Ty, Ty, bool)> = Vec::new();
        for &overridden in overridden_functions.get(&fid).into_iter().flatten() {
            let matches =
                same_representation(representation, function, &ir.functions[overridden as usize]);
            let owner = ir.functions[overridden as usize]
                .dispatch_receiver
                .and_then(|owner| ir.class_id_by_name(owner))
                .ok_or_else(|| {
                    format!(
                        "an override of a method declared outside this file (`{}`)",
                        function.name
                    )
                })?;
            let key = function_key(ir, owner, overridden);
            if ir.classes[owner as usize].is_interface {
                // The interface's number is placed program-wide rather than in this vtable, so
                // there is no entry HERE to put a bridge in. The bridge is recorded against the
                // interface's own key instead, and read where that number is filled — which is
                // the arrangement a property accessor's interface bridge already uses.
                interface_keys.push((key, (!matches).then_some(overridden)));
                continue;
            }
            let slot = slots.get(&key).copied().ok_or_else(|| {
                format!("an override with no slot to replace (`{}`)", function.name)
            })?;
            if matches {
                class_replaces = Some(slot);
            } else {
                bridged.push((slot, overridden));
            }
        }
        let replaces = match replaces {
            Some(slot) => Some(slot),
            None => match class_replaces {
                Some(slot) => Some(slot),
                None => match &own_key {
                    SlotKey::Getter(target) | SlotKey::Setter(target) => {
                        let setter = matches!(own_key, SlotKey::Setter(..));
                        let implemented = property_type(ir, *target, &overridden_properties)
                            .ok_or_else(|| {
                                format!(
                                    "a property accessor without a recorded semantic type (`{}.{}`)",
                                    class.fq_name(),
                                    function.name
                                )
                            })?;
                        match overridden_property_slot(
                            ir,
                            &overridden_properties,
                            &slots,
                            *target,
                            setter,
                            &class.fq_name(),
                        )? {
                            // The base declares the property with a different REPRESENTATION, so
                            // its slot cannot hold this accessor. This one takes a slot of its own
                            // and the base's gets a bridge, exactly as a method's does.
                            Some((slot, declared))
                                if !representation.same(declared, implemented) =>
                            {
                                accessor_bridged.push((slot, declared, implemented, setter));
                                None
                            }
                            Some((slot, _)) => Some(slot),
                            None => None,
                        }
                    }
                    _ => None,
                },
            },
        };
        let slot = match replaces {
            Some(slot) => {
                vtable[slot as usize] = entry;
                slot
            }
            None => {
                vtable.push(entry);
                (vtable.len() - 1) as u32
            }
        };
        slots.insert(own_key, slot);
        // The FUNCTION SLOT, once this method's own is known — reserved when the table began, so
        // the two are never the same number.
        if let Some(arity) = function_bridge {
            debug_assert_ne!(
                slot, FUNCTION_SLOT,
                "the stand-in forwards through its own number"
            );
            vtable[FUNCTION_SLOT as usize] = Slot::FunctionBridge {
                arity,
                target_slot: slot,
                target: fid,
            };
        }
        for (key, bridge) in interface_keys {
            // The number wears the INTERFACE's signature and converts; the slot map still points
            // at this method, because everything else that reads the map wants the
            // implementation. Which of the two a caller gets is decided where the number is
            // filled, and it prefers the bridge.
            if let Some(declared) = bridge {
                interface_bridges
                    .entry(key.clone())
                    .or_insert(Slot::Bridge {
                        declared,
                        // An INTERFACE's entry is a default an implementor inherits, and this
                        // slot is the interface's own table index — not that implementor's.
                        target_slot: (!class.is_interface).then_some(slot),
                        target: fid,
                    });
            }
            slots.insert(key, slot);
        }
        // The base keeps its own slot and its own signature; what changes is only what stands in
        // it. Forwarding by DISPATCH rather than to this method by id is what keeps a further
        // subclass's override reachable through the same base.
        for (base_slot, declared) in bridged {
            vtable[base_slot as usize] = Slot::Bridge {
                declared,
                target_slot: Some(slot),
                target: fid,
            };
        }
        for (base_slot, declared, implemented, setter) in accessor_bridged {
            vtable[base_slot as usize] = Slot::AccessorBridge {
                declared,
                implemented,
                receiver: None,
                setter,
                target_slot: slot,
            };
        }
    }

    // Open or overriding properties with no source accessor still dispatch: synthesize the
    // field access as a slot.
    for (property_index, property) in class.properties.iter().enumerate() {
        let target = local_property_target(ir, id, property_index);
        let overrides = target.is_some_and(|target| overridden_properties.contains_key(&target));
        // A property a SUBCLASS hands to an interface is dispatched through as well, whether or
        // not it is `open` here: `class B : C, A<Int>()` implements `C.size` with the `size` that
        // `A` declares plainly, and the interface's number has to reach it. Kotlin needs no
        // `open` for that — B overrides nothing — so the declaration alone cannot say it.
        if !property.is_open
            && !overrides
            && !target.is_some_and(|target| implements_an_interface(ir, target))
        {
            continue;
        }
        let target = target.ok_or_else(|| {
            format!(
                "a virtual property without a stable declaration identity (`{}.{}`)",
                class.fq_name(),
                property.name
            )
        })?;
        let accessors = [
            (false, property.getter.is_none()),
            (true, property.is_var && property.setter.is_none()),
        ];
        for (setter, needed) in accessors {
            if !needed {
                continue;
            }
            let entry = match property.backing_field {
                Some(field) if setter => Slot::FieldSetter { class: id, field },
                Some(field) => Slot::FieldGetter { class: id, field },
                None => Slot::Abstract,
            };
            let key = if setter {
                SlotKey::Setter(target)
            } else {
                SlotKey::Getter(target)
            };
            let replaces = overridden_property_slot(
                ir,
                &overridden_properties,
                &slots,
                target,
                setter,
                &class.fq_name(),
            )?;
            // A base declaring a different REPRESENTATION keeps its slot and takes a bridge, as
            // above: a synthesized field access is exactly as unusable through the base's carrier
            // as a source accessor would be.
            let bridged = match replaces {
                Some((slot, declared)) if !representation.same(declared, property.ty) => {
                    Some((slot, declared))
                }
                _ => None,
            };
            let slot = match replaces.filter(|_| bridged.is_none()) {
                Some((slot, _)) => {
                    vtable[slot as usize] = entry;
                    slot
                }
                None => {
                    vtable.push(entry);
                    (vtable.len() - 1) as u32
                }
            };
            if let Some((base_slot, declared)) = bridged {
                vtable[base_slot as usize] = Slot::AccessorBridge {
                    declared,
                    implemented: property.ty,
                    receiver: None,
                    setter,
                    target_slot: slot,
                };
            }
            slots.insert(key, slot);
            for overridden in overridden_properties.get(&target).into_iter().flatten() {
                if !overridden.interface {
                    continue;
                }
                let key = if setter {
                    SlotKey::Setter(overridden.target)
                } else {
                    SlotKey::Getter(overridden.target)
                };
                slots.insert(key, slot);
            }
        }
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

/// Point an interface's member numbers at implementations this class INHERITS rather than declares.
///
/// Kotlin calls this a fake override: `class B : A(), I` satisfies `I.foo` with `A.foo`, and `A`
/// knows nothing about `I`. `A.foo` already occupies a slot in this class's table — inherited with
/// the rest of `A`'s — so what is missing is only the interface's number pointing at that slot,
/// which nothing in the loop over the class's OWN members could have added. The frontend records
/// the edge on the class that brings the two together, which is this one.
fn register_inherited_interface_members(
    representation: &dyn Representation,
    ir: &IrFile,
    class: &IrClass,
    slots: &mut HashMap<SlotKey, u32>,
    interface_bridges: &mut HashMap<SlotKey, Slot>,
) -> Result<(), Unsupported> {
    let module_function = |target: &ResolvedFunctionOverrideTarget| match target {
        ResolvedFunctionOverrideTarget::Module(callable) => {
            ir.checked_callable_functions.get(callable).copied()
        }
        ResolvedFunctionOverrideTarget::External(_) => None,
    };
    for edge in ir
        .function_overrides
        .get(&class.fq_name_id())
        .into_iter()
        .flatten()
    {
        if !edge.overridden_is_interface {
            continue;
        }
        let implementation = edge
            .implementation_function
            .or_else(|| module_function(&edge.implementation));
        let (Some(implementation), Some(overridden)) =
            (implementation, module_function(&edge.overridden))
        else {
            continue;
        };
        let (Some(owner), Some(interface)) = (
            ir.class_id_by_name(edge.implementation_owner),
            ir.class_id_by_name(edge.overridden_owner),
        ) else {
            continue;
        };
        let Some(&slot) = slots.get(&function_key(ir, owner, implementation)) else {
            continue;
        };
        // The inherited method has to be CALLABLE through the interface's signature. `class F5 :
        // F3, D4()` where `D4.foo(): Int` is what `D1.foo(): Any` gets is the case that says why:
        // one returns an unboxed machine integer, the other a reference, and pointing the
        // interface's number straight at it would have a caller read an integer as a pointer. The
        // number takes a bridge wearing the interface's carrier instead — the same answer the
        // property path below gives, and the same one the JVM gives by emitting a bridge method.
        let key = function_key(ir, interface, overridden);
        if !same_representation(
            representation,
            &ir.functions[implementation as usize],
            &ir.functions[overridden as usize],
        ) {
            interface_bridges
                .entry(key.clone())
                .or_insert(Slot::Bridge {
                    declared: overridden,
                    target_slot: Some(slot),
                    target: implementation,
                });
        }
        slots.entry(key).or_insert(slot);
    }
    for edge in ir
        .property_overrides
        .get(&class.fq_name_id())
        .into_iter()
        .flatten()
    {
        if !edge.overridden_is_interface {
            continue;
        }
        // The same representation question the METHOD path above asks, for the same reason.
        // `interface C<T> { var size: T }` implemented by `class B : C<Int>, A()` where `A`
        // declares `var size: Int` erases the interface's accessors to a REFERENCE while the
        // inherited ones are an unboxed machine integer. Aliasing the interface's number onto them
        // would have a caller read that integer as a pointer; the number takes a bridge wearing
        // the interface's carrier instead. A property is what `bridges/test7.kt` is about, which
        // is why the method check did not catch it.
        // A member extension's receiver is an operand like any other, and crosses the same way.
        let receiver = edge.declared_receiver.zip(edge.implementation_receiver);
        let bridged = !representation.same(edge.implementation_type, edge.declared_type)
            || receiver
                .is_some_and(|(declared, implemented)| !representation.same(declared, implemented));
        for setter in [false, true] {
            let (from, to) = if setter {
                (
                    SlotKey::Setter(edge.implementation),
                    SlotKey::Setter(edge.overridden),
                )
            } else {
                (
                    SlotKey::Getter(edge.implementation),
                    SlotKey::Getter(edge.overridden),
                )
            };
            if let Some(&slot) = slots.get(&from) {
                if bridged {
                    interface_bridges
                        .entry(to.clone())
                        .or_insert(Slot::AccessorBridge {
                            declared: edge.declared_type,
                            implemented: edge.implementation_type,
                            receiver,
                            setter,
                            target_slot: slot,
                        });
                }
                slots.entry(to).or_insert(slot);
            }
        }
    }
    Ok(())
}

fn external_invoke_slot(
    representation: &dyn Representation,
    edge: &crate::ir::IrFunctionOverride,
    function: &IrFunction,
) -> Option<u32> {
    if edge.overridden_semantic_role != Some(crate::types::SemanticCallRole::KotlinFunctionInvoke) {
        return None;
    }
    (function
        .params
        .iter()
        .all(|param| representation.is_reference(*param))
        && representation.is_reference(function.ret))
    .then_some(FUNCTION_SLOT)
}

/// Whether an override is callable through the overridden slot's signature as it stands.
///
/// Kotlin permits a covariant return (`A` → `B`, both references), which changes nothing here, and
/// generic specialization (`T` → `Int`), which changes the machine representation. The second needs
/// a [`Slot::Bridge`] in the base's slot; this is the question that says which.
fn same_representation(
    representation: &dyn Representation,
    implementation: &IrFunction,
    overridden: &IrFunction,
) -> bool {
    implementation.params.len() == overridden.params.len()
        && implementation
            .params
            .iter()
            .zip(&overridden.params)
            .all(|(a, b)| representation.same(*a, *b))
        && representation.same(implementation.ret, overridden.ret)
}
