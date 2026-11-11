//! Constructor-prefix transport proved by common IR, separately from physical field ordinals.

use krusty::fir::{ClassCaptureIdentity, EnclosingInstanceKind};
use krusty::ir::{ClassId, IrCapturedReceiver, IrCtorArg, IrCtorParameterProvenance, IrFile};
use krusty::types::{Ty, TyKind, TypeName};

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
                        Some(receiver) => captured_receiver(ir, argument, receiver),
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

// A captured receiver is an ordinary reference only after its selected role and closure identity
// survived checking together. An enclosing instance proves its declaration. A context parameter
// proves the type list and index of its rung. A receiver lambda proves that same closure identity
// and an object type: that is how `with`'s receiver carries a context parameter into a local class.
// A field ordinal alone proves none of them.
fn captured_receiver(ir: &IrFile, argument: &IrCtorArg, receiver: &IrCapturedReceiver) -> bool {
    match receiver {
        IrCapturedReceiver::Enclosing { .. } => enclosing_receiver(ir, argument, receiver),
        IrCapturedReceiver::Context { types, index, .. } => {
            context_parameter(argument, types, *index)
        }
        IrCapturedReceiver::Lambda(_) => receiver_lambda(argument),
        IrCapturedReceiver::Callable { .. } => false,
    }
}

// The parameter's declared type is the entry its rung recorded at `index`. Its closure identity is
// the receiver id, not a lexical value or an enclosing class.
fn context_parameter(argument: &IrCtorArg, types: &[Ty], index: u32) -> bool {
    types
        .get(index as usize)
        .is_some_and(|declared| argument.ty == *declared)
        && matches!(
            argument.capture_identity,
            Some(ClassCaptureIdentity::Receiver(_))
        )
}

// A receiver lambda's value is the object the checker selected. Extension-callable receivers are
// a different profile and stay declined.
fn receiver_lambda(argument: &IrCtorArg) -> bool {
    matches!(
        argument.capture_identity,
        Some(ClassCaptureIdentity::Receiver(_))
    ) && matches!(argument.ty.kind(), TyKind::Obj(_, _))
}

// A source enclosing receiver is an ordinary reference only after its selected declaration,
// semantic classifier and closure identity survived checking together.
fn enclosing_receiver(ir: &IrFile, argument: &IrCtorArg, receiver: &IrCapturedReceiver) -> bool {
    let IrCapturedReceiver::Enclosing {
        classifier,
        owner: Some(owner),
    } = receiver
    else {
        return false;
    };
    if argument.capture_identity.is_none()
        || !matches!(argument.ty.kind(), TyKind::Obj(name, _) if name == *classifier)
    {
        return false;
    }
    if ir
        .checked_classifier_classes
        .contains_key(&owner.declaration)
        && ir
            .checked_enum_entry_classes
            .contains_key(&owner.declaration)
    {
        return false;
    }
    let registry = match owner.kind {
        EnclosingInstanceKind::Classifier => &ir.checked_classifier_classes,
        EnclosingInstanceKind::EnumEntry => &ir.checked_enum_entry_classes,
    };
    let Some(&selected) = registry.get(&owner.declaration) else {
        return false;
    };
    let Some(class) = ir.classes.get(selected as usize) else {
        return false;
    };
    let declared_receiver = match owner.kind {
        EnclosingInstanceKind::Classifier => class.fq_name_id() == *classifier,
        EnclosingInstanceKind::EnumEntry => {
            let Some(entry) = ir.checked_enum_entry_bodies.get(&owner.declaration) else {
                return false;
            };
            let Some(enumeration) = ir.classes.get(entry.class as usize) else {
                return false;
            };
            entry.declaration == owner.declaration
                && enumeration.is_enum
                && enumeration.fq_name_id() == *classifier
                && class.is_enum_entry
                && class.superclass == *classifier
                && enumeration
                    .enum_entries
                    .get(entry.ordinal as usize)
                    .and_then(|entry| entry.subclass)
                    == Some(class.fq_name_id())
        }
    };
    declared_receiver
        && registry
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
    interface: krusty::types::Ty,
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

/// The physical field for one checked language-level inner-to-outer edge. Its coordinate
/// comes from the selected constructor role and store, never a field name or prefix position.
pub(in crate::codegen) fn enclosing_field(
    ir: &IrFile,
    inner: TypeName,
    outer: TypeName,
) -> Option<(ClassId, u32)> {
    let id = ir.class_id_by_name(inner)?;
    let class = ir.classes.get(id as usize)?;
    ir.class_id_by_name(outer)?;
    if !class.is_inner_class || !supported(ir, id) {
        return None;
    }
    let mut enclosing = class.ctor_args.iter().enumerate().filter(|(_, argument)| {
        argument.provenance == IrCtorParameterProvenance::EnclosingInstance
    });
    let (parameter, argument) = enclosing.next()?;
    if enclosing.next().is_some()
        || parameter >= class.constructor_prefix_count as usize
        || argument.ty != Ty::obj_name(outer)
        || argument.capture.is_some()
        || argument.capture_identity.is_some()
        || ir
            .class_capture_identities
            .contains_key(&(id, parameter as u32))
        || !stored_parameter(ir, id, parameter as u32, argument)
    {
        return None;
    }
    Some((id, argument.field_index?))
}

fn stored_parameter(ir: &IrFile, id: ClassId, parameter: u32, argument: &IrCtorArg) -> bool {
    let class = &ir.classes[id as usize];
    let Some(field) = argument.field_index else {
        return false;
    };
    let Some(storage) = class.fields.get(field as usize) else {
        return false;
    };
    // Common lowering records an enclosing parameter at its raw constructor carrier, while
    // its field retains the declaration's complete self application, including captured formals.
    let storage_type = if argument.provenance == IrCtorParameterProvenance::EnclosingInstance {
        let TyKind::Obj(outer, arguments) = argument.ty.kind() else {
            return false;
        };
        if !arguments.is_empty() {
            return false;
        }
        let Some(declared) = ir.declared_class_self_type(outer) else {
            return false;
        };
        declared
    } else {
        argument.ty
    };
    if argument.anonymous_super_forward.is_some()
        || !argument.is_field
        || !storage.is_final()
        || storage.ty != storage_type
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

#[cfg(test)]
mod enclosing_instance_tests;
