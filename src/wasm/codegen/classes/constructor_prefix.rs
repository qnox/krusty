//! Constructor-prefix transport proved by common IR, separately from physical field ordinals.

use crate::fir::{ClassCaptureIdentity, EnclosingInstanceKind};
use crate::ir::{ClassId, IrCapturedReceiver, IrCtorArg, IrCtorParameterProvenance, IrFile};
use crate::types::TyKind;

pub(super) fn supported(ir: &IrFile, id: ClassId) -> bool {
    let class = &ir.classes[id as usize];
    // These edges carry a holder, while their logical Ty describes its element. The ordinary
    // constructor/field operations do not implement that cross-body holder ABI.
    if ir
        .shared_class_capture_fields
        .keys()
        .any(|(owner, _)| *owner == id)
        || ir
            .shared_super_capture_parameters
            .keys()
            .any(|(owner, _)| *owner == id)
        || ir
            .shared_secondary_super_capture_parameters
            .keys()
            .any(|(owner, _, _)| *owner == id)
    {
        return false;
    }
    let Some(prefix) = class
        .ctor_args
        .get(..class.constructor_prefix_count as usize)
    else {
        return false;
    };
    // Preserve the established inner-instance path. Adding a Capture role or payload requires
    // the complete prefix proof below even when the class is also an inner class.
    if class.is_inner_class
        && prefix.iter().all(|argument| {
            !matches!(
                argument.provenance,
                IrCtorParameterProvenance::Capture
                    | IrCtorParameterProvenance::InterfaceDelegate { .. }
            ) && argument.capture.is_none()
                && argument.capture_identity.is_none()
        })
    {
        return true;
    }
    let mut forwarded = 0;
    prefix.iter().enumerate().all(|(index, argument)| {
        let parameter = index as u32;
        match argument.provenance {
            IrCtorParameterProvenance::Value => {
                forwarded += 1;
                argument.anonymous_super_forward == Some(forwarded)
                    && argument.capture.is_none()
                    && argument.capture_identity.is_none()
                    && !ir.class_capture_identities.contains_key(&(id, parameter))
                    && !argument.is_field
                    && argument.field_index.is_none()
            }
            IrCtorParameterProvenance::InterfaceDelegate { interface, field } => {
                delegate_parameter(ir, id, parameter, argument, interface, field)
            }
            IrCtorParameterProvenance::Capture => {
                argument
                    .capture
                    .as_ref()
                    .is_some_and(|capture| match capture.receiver.as_ref() {
                        None => !matches!(
                            argument.capture_identity,
                            Some(ClassCaptureIdentity::Receiver(_))
                        ),
                        Some(receiver) => enclosing_receiver(ir, argument, receiver),
                    })
                    && argument.capture_identity
                        == ir.class_capture_identities.get(&(id, parameter)).copied()
                    && capture_parameter(ir, id, parameter, argument)
            }
            IrCtorParameterProvenance::EnclosingInstance if class.is_inner_class => {
                argument.capture.is_none()
                    && argument.capture_identity.is_none()
                    && !ir.class_capture_identities.contains_key(&(id, parameter))
                    && stored_parameter(ir, id, parameter, argument)
            }
            _ => false,
        }
    })
}

// A source enclosing receiver is an ordinary reference only after its selected declaration,
// semantic classifier and closure identity survived checking together. Other receiver profiles
// still need their own transport contract; neither a field ordinal nor a type bound proves one.
fn enclosing_receiver(ir: &IrFile, argument: &IrCtorArg, receiver: &IrCapturedReceiver) -> bool {
    let IrCapturedReceiver::Enclosing {
        classifier,
        owner: Some(owner),
    } = receiver
    else {
        return false;
    };
    if owner.kind != EnclosingInstanceKind::Classifier
        || argument.capture_identity.is_none()
        || !matches!(argument.ty.kind(), TyKind::Obj(name, _) if name == *classifier)
    {
        return false;
    }
    let Some(&selected) = ir.checked_classifier_classes.get(&owner.declaration) else {
        return false;
    };
    ir.classes
        .get(selected as usize)
        .is_some_and(|class| class.fq_name_id() == *classifier)
        && ir
            .checked_classifier_classes
            .values()
            .filter(|&&class| class == selected)
            .count()
            == 1
}

// The positive producer-owned role identifies an explicit post-super initializer. Its field
// and applied interface are retained on this exact parameter; arbitrary Value prefixes stay out.
fn delegate_parameter(
    ir: &IrFile,
    id: ClassId,
    parameter: u32,
    argument: &IrCtorArg,
    interface: crate::types::Ty,
    field: u32,
) -> bool {
    let class = &ir.classes[id as usize];
    let Some(storage) = class.fields.get(field as usize) else {
        return false;
    };
    argument.capture.is_none()
        && argument.capture_identity.is_none()
        && !ir.class_capture_identities.contains_key(&(id, parameter))
        && argument.anonymous_super_forward.is_none()
        && !argument.is_field
        && argument.field_index.is_none()
        && matches!(interface.kind(), TyKind::Obj(name, _) if class.interfaces.contains_name(name))
        && class.supertypes.contains(&interface)
        && storage.is_final()
        && storage.ty == argument.ty
        && !class.pre_super_param_fields.iter().any(|(source, target)| {
            *source == parameter || *target == field
        })
        && class.ctor_args.iter().filter(|other| {
            other.field_index == Some(field)
                || matches!(other.provenance,
                    IrCtorParameterProvenance::InterfaceDelegate { field: target, .. } if target == field)
        }).count() == 1
}

fn capture_parameter(ir: &IrFile, id: ClassId, parameter: u32, argument: &IrCtorArg) -> bool {
    match (argument.is_field, argument.field_index) {
        (true, Some(_)) => stored_parameter(ir, id, parameter, argument),
        // A constructor-only capture is already an ordinary constructor parameter. It has no
        // physical field and cannot participate in either stored or forwarded prefix transport.
        (false, None) => {
            argument.anonymous_super_forward.is_none()
                && !ir.classes[id as usize]
                    .pre_super_param_fields
                    .iter()
                    .any(|(source, _)| *source == parameter)
        }
        _ => false,
    }
}

fn stored_parameter(ir: &IrFile, id: ClassId, parameter: u32, argument: &IrCtorArg) -> bool {
    let class = &ir.classes[id as usize];
    let Some(field) = argument.field_index else {
        return false;
    };
    let Some(storage) = class.fields.get(field as usize) else {
        return false;
    };
    if argument.anonymous_super_forward.is_some()
        || !argument.is_field
        || !storage.is_final()
        || storage.ty != argument.ty
        || class
            .ctor_args
            .iter()
            .filter(|other| other.field_index == Some(field))
            .count()
            != 1
    {
        return false;
    }
    // The prefix parameter, physical field and semantic capture identity are distinct. Exactly
    // one producer-owned pre-super pair must connect this parameter and field, with no alias.
    let mut stores = class
        .pre_super_param_fields
        .iter()
        .filter(|(source, target)| *source == parameter || *target == field);
    stores.next() == Some(&(parameter, field)) && stores.next().is_none()
}
