//! Which member a method or property accessor is, and what it overrides, read off the checked
//! override edges common lowering recorded — never matched by name.

use std::collections::HashMap;

use crate::fir::{ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::ir::{ClassId, FunId, IrClass, IrFile};
use crate::types::Ty;

use super::{SlotKey, Unsupported};

/// The fixed `kotlin.Any` slot named by one exact checked override edge.
pub(crate) fn any_slot(role: Option<crate::types::SemanticCallRole>) -> Option<u32> {
    match role {
        Some(crate::types::SemanticCallRole::KotlinAnyEquals) => Some(0),
        Some(crate::types::SemanticCallRole::KotlinAnyHashCode) => Some(1),
        Some(crate::types::SemanticCallRole::KotlinAnyToString) => Some(2),
        Some(
            crate::types::SemanticCallRole::KotlinComparableCompareTo
            | crate::types::SemanticCallRole::KotlinFunctionInvoke
            | crate::types::SemanticCallRole::KotlinCallableReferenceName
            | crate::types::SemanticCallRole::KotlinPropertyReferenceGet(_)
            | crate::types::SemanticCallRole::KotlinPropertyReferenceSet(_)
            | crate::types::SemanticCallRole::KotlinPropertyReferenceDelegateGet(_)
            | crate::types::SemanticCallRole::KotlinPropertyReferenceDelegateSet(_),
        ) => None,
        None => None,
    }
}

pub(crate) fn property_name(ir: &IrFile, target: ResolvedPropertyOverrideTarget) -> String {
    match target {
        ResolvedPropertyOverrideTarget::Module(property) => ir
            .checked_properties
            .get(&property)
            .map(|property| property.name.clone())
            .unwrap_or_else(|| format!("property#{}", property.raw())),
        ResolvedPropertyOverrideTarget::External(property) => {
            format!("external-property#{}", property.raw())
        }
    }
}

/// Whether any class in this file hands this exact property to an interface it implements.
/// Such a property is reached through the interface's number and so has to dispatch, even where
/// the declaration is neither `open` nor an override — the class that implements the interface
/// declares nothing of its own.
pub(crate) fn implements_an_interface(ir: &IrFile, target: ResolvedPropertyOverrideTarget) -> bool {
    ir.property_overrides
        .values()
        .flatten()
        .any(|edge| edge.overridden_is_interface && edge.implementation == target)
}

/// The stable property identity attached to one class-property coordinate by common lowering.
pub(crate) fn local_property_target(
    ir: &IrFile,
    owner: ClassId,
    property: usize,
) -> Option<ResolvedPropertyOverrideTarget> {
    if let Some(target) = ir
        .local_property_layouts
        .iter()
        .find_map(|(identity, layout)| match layout {
            crate::ir::IrLocalPropertyLayout::Member {
                class,
                property: index,
                ..
            } if *class == owner
                && usize::try_from(*index).expect("a property index fits usize") == property =>
            {
                Some(ResolvedPropertyOverrideTarget::Module(*identity))
            }
            _ => None,
        })
    {
        return Some(target);
    }

    // Interface delegation generates an accessor body and an `IrProperty`, but no new source
    // property declaration: its exact declaration identity is the interface target carried by
    // the override edge. Bind the generated property back through those accessor identities. A
    // name is deliberately insufficient here — unrelated interfaces may declare the same one.
    let declaration = ir.classes.get(owner as usize)?.properties.get(property)?;
    let owner = ir.classes[owner as usize].fq_name_id();
    let mut found = None;
    for edge in ir.property_overrides.get(&owner).into_iter().flatten() {
        let realizes_getter = declaration
            .getter
            .is_some_and(|getter| edge.implementation_getter == Some(getter));
        let realizes_setter = declaration
            .setter
            .is_some_and(|setter| edge.implementation_setter == Some(setter));
        if !realizes_getter && !realizes_setter {
            continue;
        }
        assert!(
            found.is_none_or(|target| target == edge.implementation),
            "one generated property cannot realize two property declarations"
        );
        found = Some(edge.implementation);
    }
    found
}

/// The exact property accessor realized by one common-IR function, if any.
pub(crate) fn property_accessor_key(
    ir: &IrFile,
    owner: ClassId,
    function: FunId,
) -> Option<SlotKey> {
    let owner_name = ir.classes[owner as usize].fq_name_id();
    let mut found = None;
    let mut record = |key: SlotKey| {
        if let Some(existing) = &found {
            assert_eq!(
                existing, &key,
                "one common-IR function cannot realize two property declarations"
            );
        } else {
            found = Some(key);
        }
    };
    for (identity, layout) in &ir.local_property_layouts {
        let accessors = match layout {
            crate::ir::IrLocalPropertyLayout::Member {
                class,
                getter,
                setter,
                ..
            } if *class == owner => Some((*getter, *setter)),
            crate::ir::IrLocalPropertyLayout::MemberExtension {
                owner,
                getter,
                setter,
                ..
            } if *owner == owner_name => Some((Some(*getter), *setter)),
            _ => None,
        };
        let Some((getter, setter)) = accessors else {
            continue;
        };
        if getter == Some(function) {
            record(SlotKey::Getter(ResolvedPropertyOverrideTarget::Module(
                *identity,
            )));
        }
        if setter == Some(function) {
            record(SlotKey::Setter(ResolvedPropertyOverrideTarget::Module(
                *identity,
            )));
        }
    }
    for edge in ir.property_overrides.get(&owner_name).into_iter().flatten() {
        if edge.implementation_getter == Some(function) {
            record(SlotKey::Getter(edge.implementation));
        }
        if edge.implementation_setter == Some(function) {
            record(SlotKey::Setter(edge.implementation));
        }
    }
    found
}

/// The slot key of a method of `owner`: a property accessor is keyed by its property.
pub(crate) fn function_key(ir: &IrFile, owner: ClassId, fid: FunId) -> SlotKey {
    property_accessor_key(ir, owner, fid).unwrap_or(SlotKey::Function(fid))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PropertyOverrideSlot {
    pub(crate) target: ResolvedPropertyOverrideTarget,
    pub(crate) declared: Ty,
    pub(crate) implemented: Ty,
    pub(crate) interface: bool,
}

pub(crate) type PropertyOverrideSlots =
    HashMap<ResolvedPropertyOverrideTarget, Vec<PropertyOverrideSlot>>;

pub(crate) fn property_type(
    ir: &IrFile,
    target: ResolvedPropertyOverrideTarget,
    overrides: &PropertyOverrideSlots,
) -> Option<Ty> {
    overrides
        .get(&target)
        .and_then(|targets| targets.first())
        .map(|edge| edge.implemented)
        .or_else(|| match target {
            ResolvedPropertyOverrideTarget::Module(property) => ir
                .checked_properties
                .get(&property)
                .map(|property| property.ty),
            ResolvedPropertyOverrideTarget::External(_) => None,
        })
}

/// The slot an overriding property accessor replaces, found through the overridden property's
/// declaring class.
pub(crate) fn overridden_property_slot(
    ir: &IrFile,
    overridden: &PropertyOverrideSlots,
    slots: &HashMap<SlotKey, u32>,
    implementation: ResolvedPropertyOverrideTarget,
    setter: bool,
    class_name: &str,
) -> Result<Option<(u32, Ty)>, Unsupported> {
    // Only a CLASS base owns a slot to replace. An interface base owns a number in the program-wide
    // interface region instead, and that is pointed at this property afterwards rather than
    // replaced here.
    let Some(overridden) = overridden
        .get(&implementation)
        .and_then(|targets| targets.iter().find(|target| !target.interface))
    else {
        return Ok(None);
    };
    let key = if setter {
        SlotKey::Setter(overridden.target)
    } else {
        SlotKey::Getter(overridden.target)
    };
    // A `val` overridden by a `var` adds a setter the base never had: a new slot, not an override.
    if setter && !slots.contains_key(&key) {
        return Ok(None);
    }
    // The base's declared type travels on the exact override edge, because whether the slot can simply be
    // REPLACED depends on it: a base declaring `var size: T` carries a reference where an
    // overriding `var size: Int` carries a machine integer, and replacing then has a caller
    // reading the base's slot read that integer as a pointer.
    slots
        .get(&key)
        .copied()
        .map(|slot| (slot, overridden.declared))
        .map(Some)
        .ok_or_else(|| {
            format!(
                "a property override with no slot to replace (`{class_name}.{}`)",
                property_name(ir, implementation)
            )
        })
}

/// Implementation method → the method it overrides, for the methods `class` declares. Both ends
/// must be functions of this file.
pub(crate) fn overridden_functions(
    ir: &IrFile,
    class: &IrClass,
) -> Result<HashMap<FunId, Vec<FunId>>, Unsupported> {
    let mut map: HashMap<FunId, Vec<FunId>> = HashMap::new();
    let Some(overrides) = ir.function_overrides.get(&class.fq_name_id()) else {
        return Ok(map);
    };
    for edge in overrides {
        let implementation = match (&edge.implementation, edge.implementation_function) {
            (_, Some(fid)) => Some(fid),
            (ResolvedFunctionOverrideTarget::Module(callable), None) => {
                ir.checked_callable_functions.get(callable).copied()
            }
            (ResolvedFunctionOverrideTarget::External(_), None) => None,
        };
        let Some(implementation) = implementation else {
            continue;
        };
        let function = &ir.functions[implementation as usize];
        match &edge.overridden {
            ResolvedFunctionOverrideTarget::Module(callable) => {
                match ir.checked_callable_functions.get(callable) {
                    Some(&overridden) => {
                        // EVERY target, not just the nearest: one method can override its
                        // superclass's and an interface's at once, and the two want different
                        // things — one slot replaced, one interface number pointed here.
                        let targets = map.entry(implementation).or_default();
                        if !targets.contains(&overridden) {
                            targets.push(overridden);
                        }
                    }
                    None => {
                        return Err(format!(
                            "an override of a method declared in another file (`{}.{}`)",
                            class.fq_name(),
                            function.name
                        ))
                    }
                }
            }
            ResolvedFunctionOverrideTarget::External(_) => {
                // An override of a DEPENDENCY method takes a slot of its own, like any method
                // this class declares freshly: a caller naming the class reaches it, and a caller
                // naming the dependency type declines at the call site, where the type it named
                // is still in sight.
                //
                // `invoke` is the exception, because it is the one dependency member this target
                // already gives a FIXED number: a function value's body sits right after
                // `kotlin.Any`'s three and the runtime names that number itself
                // (`KT_SLOT_INVOKE`), so every caller through a function type reads it rather
                // than asking. One that cannot take that slot outright — its operands are not all
                // references — gets a `Slot::FunctionBridge` there instead, placed where the
                // method's own slot is known; nothing is recorded here.
            }
        }
    }
    Ok(map)
}

/// Exact implementation property → the declarations it overrides in this file.
pub(crate) fn overridden_properties(
    ir: &IrFile,
    id: ClassId,
    class: &IrClass,
) -> Result<PropertyOverrideSlots, Unsupported> {
    let mut map: PropertyOverrideSlots = HashMap::new();
    let Some(overrides) = ir.property_overrides.get(&class.fq_name_id()) else {
        return Ok(map);
    };
    for edge in overrides {
        let implemented_here = edge.implementation_getter.is_some()
            || edge.implementation_setter.is_some()
            || match edge.implementation {
                ResolvedPropertyOverrideTarget::Module(property) => ir
                    .checked_properties
                    .get(&property)
                    .is_some_and(|property| property.class == Some(id)),
                ResolvedPropertyOverrideTarget::External(_) => false,
            };
        if !implemented_here {
            continue;
        }
        // An override of a property declared OUTSIDE this file takes a slot of its own, like any
        // property this class declares freshly: a caller naming the class reaches it, and a caller
        // naming the dependency type declines at the call site, where the type it named is still
        // in sight. There is no base slot here to replace, and nothing in this file numbers one.
        if !matches!(edge.overridden, ResolvedPropertyOverrideTarget::Module(_)) {
            continue;
        }
        // Every target: a property can override a superclass's and an interface's at once, and
        // the direct base's edge (depth 1) is the one whose slot is replaced, so it goes first.
        let overridden = PropertyOverrideSlot {
            target: edge.overridden,
            declared: edge.declared_type,
            implemented: edge.implementation_type,
            interface: edge.overridden_is_interface,
        };
        let targets = map.entry(edge.implementation).or_default();
        if !targets.contains(&overridden) {
            if edge.depth == 1 {
                targets.insert(0, overridden);
            } else {
                targets.push(overridden);
            }
        }
    }
    Ok(map)
}
