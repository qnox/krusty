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
//! in declaration order. A private constructor's is planned with the class's other synthetic
//! accessors (`static_accessors`), at the first call from another class.

use super::constructor_defaults::emit_ctor_marker_accessor;
use super::{class_ctor_jvm_tys, jvm_tys, ClassWriter};
use crate::ir::{
    ClassId, CtorDelegateTarget, IrClass, IrConstructorAccess, IrConstructorTarget, IrFile,
};
use crate::jvm::method_parameters::{
    primary_constructor_identities, secondary_constructor_identities, OwnerConstructorPrefix,
};
use crate::types::TypeName;

/// Whether this construction goes through the value-class `DefaultConstructorMarker` accessor.
///
/// An annotation implementation stores each member as its carrier. Its `<init>` does not take
/// that marker, even when a member is an unsigned array.
pub(super) fn uses_value_class_marker_accessor(
    ir: &IrFile,
    expression: crate::ir::ExprId,
    owner: TypeName,
    owner_spelling: &str,
    defaults_empty: bool,
    parameters_come_from_class: bool,
) -> bool {
    if !defaults_empty {
        return false;
    }
    if ir
        .class_id_by_name(owner)
        .is_some_and(|class| ir.classes[class as usize].annotation_impl_of.is_some())
    {
        return false;
    }
    (parameters_come_from_class && ir.has_value_param_ctor(owner_spelling))
        || ir.has_value_class_parameter_construction(expression)
        || ir
            .construction_targets
            .get(&expression)
            .is_some_and(|target| {
                reached_through_accessor(
                    *target,
                    ir.expression_owners.get(&expression).copied(),
                    owner,
                )
            })
}

/// Whether code in `caller` calls the `owner` constructor `target` through its accessor rather
/// than directly. `caller` is `None` for package-level code.
pub(super) fn reached_through_accessor(
    target: IrConstructorTarget,
    caller: Option<TypeName>,
    owner: TypeName,
) -> bool {
    match target.access {
        IrConstructorAccess::SealedClass => true,
        IrConstructorAccess::Private => caller != Some(owner),
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

/// Emit the accessor of each constructor a sealed `class` hides. A private constructor's accessor
/// is planned with the class's other synthetic accessors, at the first call from another class.
pub(super) fn emit_sealed_accessors(
    ir: &IrFile,
    class: &IrClass,
    owner: &str,
    cw: &mut ClassWriter,
) {
    if !class.is_sealed {
        return;
    }
    let secondaries = (0..class.secondary_ctors.len())
        .filter(|&ordinal| hides_secondary(ir, class, ordinal))
        .map(|ordinal| u32::try_from(ordinal + 1).expect("constructor ordinal fits in u32"));
    for ordinal in class
        .has_primary_ctor
        .then_some(0)
        .into_iter()
        .chain(secondaries)
    {
        emit_accessor(ir, class, ordinal, owner, cw);
    }
}

pub(super) fn emit_accessor(
    ir: &IrFile,
    class: &IrClass,
    ordinal: u32,
    owner: &str,
    cw: &mut ClassWriter,
) {
    // An enum's constructors all take the constant's name and ordinal first, and so do their
    // accessors.
    let owner_prefix = if class.is_enum {
        OwnerConstructorPrefix::enum_class()
    } else {
        OwnerConstructorPrefix::none()
    };
    let Some(secondary) = ordinal.checked_sub(1) else {
        let declared = class_ctor_jvm_tys(class);
        let identities = owner_prefix
            .identities()
            .chain(primary_constructor_identities(ir, class, &declared))
            .collect::<Vec<_>>();
        let parameters = owner_prefix
            .types
            .iter()
            .copied()
            .chain(declared)
            .collect::<Vec<_>>();
        emit_ctor_marker_accessor(owner, &parameters, &identities, cw);
        return;
    };
    let constructor = &class.secondary_ctors[secondary as usize];
    let parameters = owner_prefix
        .types
        .iter()
        .copied()
        .chain(jvm_tys(&constructor.prefix_params))
        .chain(jvm_tys(&constructor.params))
        .collect::<Vec<_>>();
    let identities =
        secondary_constructor_identities(ir, class, constructor, &owner_prefix, &parameters);
    emit_ctor_marker_accessor(owner, &parameters, &identities, cw);
}

/// Private constructor `target` of a non-sealed class, as `(class, ordinal)`, when code outside
/// that class calls it through an accessor. A constructor taking a value class already has its
/// marker accessor, the only way in.
fn private_target(
    ir: &IrFile,
    owner: TypeName,
    target: IrConstructorTarget,
) -> Option<(ClassId, u32)> {
    if target.access != IrConstructorAccess::Private {
        return None;
    }
    let class_id = ir.class_id_by_name(owner)?;
    let class = &ir.classes[class_id as usize];
    let hidden_by_value_class = match target.ordinal.checked_sub(1) {
        None => ir.has_value_param_ctor_of(class.fq_name_id()),
        Some(secondary) => class.secondary_ctors[secondary as usize].vc_params,
    };
    (!class.is_sealed && !hidden_by_value_class).then_some((class_id, target.ordinal))
}

/// The private constructor construction `expression` reaches through its accessor, as `(class,
/// ordinal)`. A default-argument call goes through the `$default` overload instead.
pub(super) fn private_construction(
    ir: &IrFile,
    expression: crate::ir::ExprId,
) -> Option<(ClassId, u32)> {
    let target = *ir.construction_targets.get(&expression)?;
    let crate::ir::IrExpr::New {
        internal, defaults, ..
    } = ir.expr(expression)
    else {
        return None;
    };
    let caller = *ir.expression_owners.get(&expression)?;
    if caller == *internal || !defaults.is_empty() {
        return None;
    }
    private_target(ir, *internal, target)
}

/// Each `super(…)` delegation of another class to a private constructor, as `(line, class,
/// ordinal)`: a class's primary delegation and its secondary constructors'.
pub(super) fn private_delegations(ir: &IrFile) -> impl Iterator<Item = (u32, ClassId, u32)> + '_ {
    ir.classes.iter().flat_map(move |caller| {
        let primary = (super_delegation_is_direct(ir, caller)
            && caller.superclass != caller.fq_name_id())
        .then(|| private_target(ir, caller.superclass, caller.super_ctor))
        .flatten()
        .map(|(class, ordinal)| {
            (
                caller.primary_delegation_line().unwrap_or(0),
                class,
                ordinal,
            )
        });
        let secondaries = caller.secondary_ctors.iter().filter_map(move |secondary| {
            let CtorDelegateTarget::Super { owner, target, .. } = &secondary.delegate else {
                return None;
            };
            if *owner == caller.fq_name_id() || !secondary.default_parameters.is_empty() {
                return None;
            }
            private_target(ir, *owner, *target).map(|(class, ordinal)| {
                (
                    secondary.lines.delegation_call_line().unwrap_or(0),
                    class,
                    ordinal,
                )
            })
        });
        primary.into_iter().chain(secondaries)
    })
}

/// Whether a class's primary `super(…)` names the selected constructor itself rather than its
/// `$default` overload.
fn super_delegation_is_direct(ir: &IrFile, class: &IrClass) -> bool {
    ir.super_constructor_default_arguments
        .get(&class.fq_name_id())
        .is_none_or(Vec::is_empty)
}
