//! kotlinc hides every source constructor of a sealed class.
//!
//! The class file declares each one private and pairs it with a public synthetic
//! `(…, DefaultConstructorMarker)` accessor, the only way in: a subclass's `super(…)` and the
//! class's own `this(…)` both call the accessor. The accessors follow the class's members and
//! bridges, one per constructor in declaration order.

use super::constructor_defaults::emit_ctor_marker_accessor;
use super::{class_ctor_jvm_tys, jvm_tys, ClassWriter};
use crate::ir::{IrClass, IrConstructorAccess, IrConstructorTarget, IrFile};
use crate::jvm::method_parameters::{
    primary_constructor_identities, secondary_constructor_identities, OwnerConstructorPrefix,
};

/// Whether a delegation to `target` calls its accessor rather than the constructor itself.
pub(super) fn reached_through_accessor(target: IrConstructorTarget) -> bool {
    target.access == IrConstructorAccess::SealedClass
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

/// Emit the accessor of each constructor a sealed class hides.
pub(super) fn emit_accessors(ir: &IrFile, class: &IrClass, owner: &str, cw: &mut ClassWriter) {
    if !class.is_sealed {
        return;
    }
    if class.has_primary_ctor {
        let parameters = class_ctor_jvm_tys(class);
        let identities = primary_constructor_identities(class, &parameters);
        emit_ctor_marker_accessor(owner, &parameters, &identities, cw);
    }
    for (ordinal, constructor) in class.secondary_ctors.iter().enumerate() {
        if !hides_secondary(ir, class, ordinal) {
            continue;
        }
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
}
