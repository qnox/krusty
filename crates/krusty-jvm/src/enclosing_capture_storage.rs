//! Realize a checked enclosing instance without changing its semantic enum view.
//!
//! An enum-entry body has the enum's source type, but its captured instance is stored using the
//! exact entry subclass. The checker retains that declaration identity across recapture; this
//! boundary couples the field, constructor prefix and selected local construction signatures.

use krusty::fir::EnclosingInstanceKind;
use krusty::fx_hash::HashMap;
use krusty::ir::{
    CtorDelegateTarget, IrCapturedReceiver, IrCtorParameterProvenance, IrExpr, IrFile,
};
use krusty::types::{Ty, TypeName};

/// The selected enclosing instance's JVM representation; its source type stays separate.
/// Unknown external owners retain their existing semantic view rather than selecting an entry.
pub(super) fn selected_receiver_type(
    ir: &IrFile,
    receiver: &IrCapturedReceiver,
    semantic: Ty,
) -> Result<Option<Ty>, String> {
    let IrCapturedReceiver::Enclosing {
        classifier,
        owner: Some(owner),
    } = receiver
    else {
        return Ok(None);
    };
    if owner.kind != EnclosingInstanceKind::EnumEntry {
        if ir
            .checked_enum_entry_classes
            .contains_key(&owner.declaration)
        {
            return Err("an enum-entry instance has a classifier-owner role".into());
        }
        return Ok(None);
    }
    let class = ir
        .checked_enum_entry_classes
        .get(&owner.declaration)
        .copied();
    let entry = class
        .and_then(|class| ir.classes.get(class as usize))
        .filter(|class| class.is_enum_entry)
        .ok_or_else(|| {
            format!(
                "enclosing capture owner {:?} has no realized enum-entry class",
                owner.declaration
            )
        })?;
    if ir
        .checked_enum_entry_classes
        .values()
        .filter(|&&other| Some(other) == class)
        .count()
        != 1
    {
        return Err("an enclosing capture class has multiple selected entry owners".into());
    }
    if entry.superclass != *classifier || semantic != Ty::obj_name(*classifier) {
        return Err("an enclosing capture disagrees with its selected enum owner".into());
    }
    Ok(Some(Ty::obj_name(entry.fq_name_id())))
}

#[derive(Clone, Copy)]
struct CaptureSlot {
    parameter: usize,
    field: Option<usize>,
    semantic: Ty,
    physical: Ty,
}

pub(super) fn realize(ir: &mut IrFile) -> Result<(), String> {
    let mut layouts = HashMap::<TypeName, Vec<CaptureSlot>>::default();
    for class in &ir.classes {
        let mut slots = Vec::new();
        for (parameter, argument) in class.ctor_args.iter().enumerate() {
            let Some(
                receiver @ IrCapturedReceiver::Enclosing {
                    owner: Some(owner), ..
                },
            ) = argument
                .capture
                .as_ref()
                .and_then(|capture| capture.receiver.as_ref())
            else {
                continue;
            };
            if owner.kind != EnclosingInstanceKind::EnumEntry {
                selected_receiver_type(ir, receiver, argument.ty)?;
                continue;
            }
            if parameter >= class.constructor_prefix_count as usize
                || argument.provenance != IrCtorParameterProvenance::Capture
            {
                return Err(format!("enclosing capture parameter {parameter} is not in its constructor capture prefix"));
            }
            let physical = selected_receiver_type(ir, receiver, argument.ty)?
                .expect("a selected enum-entry receiver has its physical representation");
            let field = argument.field_index.map(|field| field as usize);
            if argument.is_field != field.is_some()
                || field.is_some_and(|field| field >= class.fields.len())
            {
                return Err(format!(
                    "enclosing capture parameter {parameter} has no paired field"
                ));
            }
            if let Some(field) = field {
                if class.fields[field].ty != argument.ty {
                    return Err(format!(
                        "enclosing capture parameter {parameter} disagrees with its paired field type"
                    ));
                }
                if class
                    .ctor_args
                    .iter()
                    .filter(|argument| argument.field_index == Some(field as u32))
                    .count()
                    != 1
                {
                    return Err(format!(
                        "enclosing capture field {field} has multiple constructor owners"
                    ));
                }
            }
            slots.push(CaptureSlot {
                parameter,
                field,
                semantic: argument.ty,
                physical,
            });
        }
        if !slots.is_empty() {
            for constructor in &class.secondary_ctors {
                validate(&constructor.prefix_params, &slots)?;
            }
            layouts.insert(class.fq_name_id(), slots);
        }
    }

    // Authenticate every consumer before publishing any physical slot. A later malformed
    // selected construction must not leave class fields or secondary prefixes half realized.
    for class in &ir.classes {
        if let Some(slots) = layouts.get(&class.superclass) {
            validate(&class.super_ctor_params, slots)?;
        }
        for constructor in &class.secondary_ctors {
            if let CtorDelegateTarget::Super {
                owner,
                target_params,
                ..
            } = &constructor.delegate
            {
                if let Some(slots) = layouts.get(owner) {
                    validate(target_params, slots)?;
                }
            }
        }
    }
    for expression in &ir.exprs {
        let IrExpr::New {
            internal,
            ctor_params,
            ctor_desc,
            external_target,
            ..
        } = expression
        else {
            continue;
        };
        let Some(slots) = layouts.get(internal) else {
            continue;
        };
        if ctor_desc.is_some() || external_target.is_some() {
            return Err(
                "an enclosing capture construction has no typed local constructor boundary".into(),
            );
        }
        if let Some(parameters) = ctor_params {
            validate(parameters, slots)?;
        }
    }

    for class in &mut ir.classes {
        if let Some(slots) = layouts.get(&class.fq_name_id()) {
            for slot in slots {
                class.ctor_args[slot.parameter].ty = slot.physical;
                if let Some(field) = slot.field {
                    class.fields[field].ty = slot.physical;
                }
                for constructor in &mut class.secondary_ctors {
                    constructor.prefix_params[slot.parameter] = slot.physical;
                }
            }
        }
        if let Some(slots) = layouts.get(&class.superclass) {
            retarget(&mut class.super_ctor_params, slots);
        }
        for constructor in &mut class.secondary_ctors {
            // `this` forwards prefix_params separately; its target_params are source parameters.
            // A selected superclass constructor already includes its checked inherited prefix.
            if let CtorDelegateTarget::Super {
                owner,
                target_params,
                ..
            } = &mut constructor.delegate
            {
                if let Some(slots) = layouts.get(owner) {
                    retarget(target_params, slots);
                }
            }
        }
    }
    for expression in &mut ir.exprs {
        let IrExpr::New {
            internal,
            ctor_params: Some(parameters),
            ctor_desc: None,
            external_target: None,
            ..
        } = expression
        else {
            continue;
        };
        if let Some(slots) = layouts.get(internal) {
            retarget(parameters, slots);
        }
    }
    Ok(())
}

fn retarget(parameters: &mut [Ty], slots: &[CaptureSlot]) {
    // Every paired slot was authenticated before the first mutation. This publication phase
    // cannot decline after changing another field or selected constructor signature.
    for slot in slots {
        parameters[slot.parameter] = slot.physical;
    }
}

fn validate(parameters: &[Ty], slots: &[CaptureSlot]) -> Result<(), String> {
    for slot in slots {
        let Some(parameter) = parameters.get(slot.parameter) else {
            return Err(format!(
                "selected constructor has no enclosing capture prefix parameter {}",
                slot.parameter
            ));
        };
        if *parameter != slot.semantic {
            return Err(format!(
                "selected constructor enclosing capture parameter {} has a different semantic type",
                slot.parameter
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
