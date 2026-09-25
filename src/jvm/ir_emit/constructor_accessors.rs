//! The synthetic `(…, DefaultConstructorMarker)` accessors kotlinc gives a constructor that its
//! callers cannot reach directly on the JVM.
//!
//! A sealed class hides every source constructor: the class file declares each one private, and
//! its accessor is the only way in, for a subclass's `super(…)` and for the class's own `this(…)`
//! alike. A private constructor of any other class keeps direct calls from its own class and gets
//! an accessor only when another class calls it: a nested subclass's `super(…)`, a companion's
//! factory, an object expression's body. Default-argument calls reach the constructor through its
//! package-private `$default` overload instead, which needs no accessor.
//!
//! The accessors follow the class's members and bridges. A sealed class's come one per constructor
//! in declaration order; a private constructor's in the order kotlinc's source-order walk first
//! meets a call from another class.

use super::constructor_defaults::emit_ctor_marker_accessor;
use super::{class_ctor_jvm_tys, jvm_tys, ClassWriter};
use crate::ir::{CtorDelegateTarget, IrClass, IrConstructorAccess, IrConstructorTarget, IrFile};
use crate::jvm::method_parameters::{
    primary_constructor_identities, secondary_constructor_identities, OwnerConstructorPrefix,
};
use crate::types::TypeName;

/// Whether code in `caller` calls the `owner` constructor `target` through its accessor rather
/// than directly. `caller` is `None` for package-level code.
pub(super) fn reached_through_accessor(
    target: IrConstructorTarget,
    caller: Option<TypeName>,
    owner: TypeName,
) -> bool {
    match target.access {
        IrConstructorAccess::SealedClass => true,
        IrConstructorAccess::Private { .. } => caller != Some(owner),
        IrConstructorAccess::Unrestricted => false,
    }
}

/// Whether a sealed class hides its secondary constructor `ordinal`. The serialization plugin's
/// constructor is public, so kotlinc leaves it callable.
pub(super) fn hides_secondary(ir: &IrFile, class: &IrClass, ordinal: usize) -> bool {
    class.is_sealed
        && ir
            .generated_secondary_constructor_by_owner(
                class.fq_name_id(),
                crate::ir::IrSecondaryConstructorRole::SerializationDeserialization,
            )
            .is_none_or(|generated| generated as usize != ordinal)
}

/// Emit the accessor of each constructor `class` hides or another class calls privately.
pub(super) fn emit_accessors(ir: &IrFile, class: &IrClass, owner: &str, cw: &mut ClassWriter) {
    let ordinals: Vec<u32> = if class.is_sealed {
        let secondaries = (0..class.secondary_ctors.len())
            .filter(|&ordinal| hides_secondary(ir, class, ordinal))
            .map(|ordinal| u32::try_from(ordinal + 1).expect("constructor ordinal fits in u32"));
        class
            .has_primary_ctor
            .then_some(0)
            .into_iter()
            .chain(secondaries)
            .collect()
    } else {
        // A constructor taking a value class already has its marker accessor, the only way in.
        private_constructors_called_from_outside(ir, class.fq_name_id())
            .into_iter()
            .filter(|&ordinal| match ordinal.checked_sub(1) {
                None => !ir.has_value_param_ctor(owner),
                Some(secondary) => !class.secondary_ctors[secondary as usize].vc_params,
            })
            .collect()
    };
    for ordinal in ordinals {
        emit_accessor(class, ordinal, owner, cw);
    }
}

fn emit_accessor(class: &IrClass, ordinal: u32, owner: &str, cw: &mut ClassWriter) {
    let Some(secondary) = ordinal.checked_sub(1) else {
        let parameters = class_ctor_jvm_tys(class);
        let identities = primary_constructor_identities(class, &parameters);
        emit_ctor_marker_accessor(owner, &parameters, &identities, cw);
        return;
    };
    let constructor = &class.secondary_ctors[secondary as usize];
    let parameters = jvm_tys(&constructor.prefix_params)
        .into_iter()
        .chain(jvm_tys(&constructor.params))
        .collect::<Vec<_>>();
    let identities = secondary_constructor_identities(
        class,
        constructor,
        &OwnerConstructorPrefix::none(),
        &parameters,
    );
    emit_ctor_marker_accessor(owner, &parameters, &identities, cw);
}

/// The ordinals of `owner`'s private constructors that another class calls directly, in the order
/// kotlinc creates their accessors: by the calling class's source position, a class's own
/// `super(…)` before the calls in its body.
fn private_constructors_called_from_outside(ir: &IrFile, owner: TypeName) -> Vec<u32> {
    let private_ordinal = |target: IrConstructorTarget| match target.access {
        IrConstructorAccess::Private { ordinal } => Some(ordinal),
        _ => None,
    };
    // (calling class's source position, kind of call, the call within its kind, ordinal): a
    // class's `super(…)`, then its secondary constructors' by source position, then constructions
    // in lowering order. A generated class has no source position and sorts after declared ones.
    let mut calls: Vec<(u32, u8, u32, u32)> = Vec::new();
    for (class_id, caller) in ir.classes.iter().enumerate() {
        if caller.fq_name_id() == owner {
            continue;
        }
        let position = ir
            .class_source_order(class_id as crate::ir::ClassId)
            .unwrap_or(u32::MAX);
        if caller.superclass == owner && super_delegation_is_direct(ir, caller) {
            calls.extend(
                private_ordinal(caller.super_ctor).map(|ordinal| (position, 0, 0, ordinal)),
            );
        }
        for secondary in &caller.secondary_ctors {
            if let CtorDelegateTarget::Super {
                owner: target_owner,
                target,
                ..
            } = &secondary.delegate
            {
                if *target_owner == owner && secondary.default_parameters.is_empty() {
                    calls.extend(
                        private_ordinal(*target)
                            .map(|ordinal| (position, 1, secondary.source_order, ordinal)),
                    );
                }
            }
        }
    }
    for (&construction, &target) in &ir.construction_targets {
        let Some(ordinal) = private_ordinal(target) else {
            continue;
        };
        let crate::ir::IrExpr::New {
            internal, defaults, ..
        } = ir.expr(construction)
        else {
            continue;
        };
        let Some(&caller) = ir.expression_owners.get(&construction) else {
            continue;
        };
        if *internal != owner || caller == owner || !defaults.is_empty() {
            continue;
        }
        let position = ir
            .class_id_by_name(caller)
            .and_then(|class| ir.class_source_order(class))
            .unwrap_or(u32::MAX);
        calls.push((position, 2, construction, ordinal));
    }
    calls.sort_unstable();
    let mut ordinals = Vec::new();
    for (_, _, _, ordinal) in calls {
        if !ordinals.contains(&ordinal) {
            ordinals.push(ordinal);
        }
    }
    ordinals
}

/// Whether a class's primary `super(…)` names the selected constructor itself rather than its
/// `$default` overload.
fn super_delegation_is_direct(ir: &IrFile, class: &IrClass) -> bool {
    ir.super_constructor_default_arguments
        .get(&class.fq_name_id())
        .is_none_or(Vec::is_empty)
}
