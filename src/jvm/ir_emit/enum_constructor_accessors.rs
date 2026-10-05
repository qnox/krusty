//! The synthetic `(…, DefaultConstructorMarker)` accessors of an enum's constructors.
//!
//! Every enum constructor is private. An entry with a body becomes a subclass, which calls the
//! enum constructor its entry selected from another class, so kotlinc gives that constructor a
//! public synthetic accessor taking the enum's `(String, int)` prefix, the declared parameters and
//! a trailing marker. The accessors follow the enum's members and bridges, one per targeted
//! constructor in the order the entries first select them.

use super::constructor_defaults::emit_ctor_marker_accessor;
use super::{class_ctor_jvm_tys, jvm_tys, ClassWriter};
use crate::ir::{IrClass, IrFile};
use crate::jvm::method_parameters::{secondary_constructor_identities, OwnerConstructorPrefix};
use crate::types::Ty;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnumConstructor {
    Primary,
    Secondary(usize),
}

/// Emit the accessor of each constructor an entry body's subclass delegates to.
pub(super) fn emit(ir: &IrFile, class: &IrClass, owner: &str, cw: &mut ClassWriter) {
    for constructor in targeted_constructors(class) {
        match constructor {
            EnumConstructor::Primary => {
                let parameters = enum_prefixed(class_ctor_jvm_tys(class));
                let identities = ["$enum$name".to_string(), "$enum$ordinal".to_string()]
                    .into_iter()
                    .map(Some)
                    .chain(crate::jvm::parameter_names::constructor_local_variables(
                        ir, class,
                    ))
                    .collect::<Vec<_>>();
                emit_ctor_marker_accessor(owner, &parameters, &identities, cw);
            }
            EnumConstructor::Secondary(ordinal) => {
                let secondary = &class.secondary_ctors[ordinal];
                let parameters = enum_prefixed(jvm_tys(&secondary.params));
                let identities = secondary_constructor_identities(
                    ir,
                    class,
                    secondary,
                    &OwnerConstructorPrefix::enum_class(),
                    &parameters,
                );
                emit_ctor_marker_accessor(owner, &parameters, &identities, cw);
            }
        }
    }
}

fn enum_prefixed(declared: Vec<Ty>) -> Vec<Ty> {
    [Ty::String, Ty::Int].into_iter().chain(declared).collect()
}

/// The constructors entry bodies delegate to, in the order the entries first select them.
fn targeted_constructors(class: &IrClass) -> Vec<EnumConstructor> {
    let emits_primary = class.has_primary_ctor || class.secondary_ctors.is_empty();
    let primary = class_ctor_jvm_tys(class);
    let mut targets = Vec::new();
    // An entry that defaults an argument calls the constructor's default-argument overload,
    // which is already reachable.
    for entry in class
        .enum_entries
        .iter()
        .filter(|entry| entry.subclass.is_some() && entry.default_parameters.is_empty())
    {
        let selected = jvm_tys(&entry.constructor_parameter_types);
        let target = if emits_primary && selected == primary {
            Some(EnumConstructor::Primary)
        } else {
            class
                .secondary_ctors
                .iter()
                .position(|secondary| jvm_tys(&secondary.params) == selected)
                .map(EnumConstructor::Secondary)
        };
        if let Some(target) = target.filter(|target| !targets.contains(target)) {
            targets.push(target);
        }
    }
    targets
}
