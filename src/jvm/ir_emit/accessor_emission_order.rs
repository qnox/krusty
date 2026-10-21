//! Relative classfile order of constructor and static accessors.
//!
//! A companion's synthetic marker constructor splits its ordinary property/static accessors from
//! companion-instance accessors. Other classes emit all static accessors together, while a hidden
//! value-class primary can still place its marker constructor after them. kotlinc's accessor
//! lowering adds a serializable class's `access$get$childSerializers$cp` after that primary
//! marker and before a secondary constructor's.

use super::{
    class_ctor_jvm_tys, constructor_accessors, constructor_defaults, emit_method,
    has_ctor_marker_accessor, marker_accessor_emitted_last, static_accessors, ClassWriter, EmitEnv,
    StaticOwner,
};
use crate::ir::{FunId, IrClass, IrFile};
use crate::types::Visibility;

/// The child-serializer cache's public `…$cp` accessor when it follows the hidden value-class
/// primary's marker accessor instead of the cache's factories.
pub(super) fn deferred_cache_accessor(ir: &IrFile, class: &IrClass) -> Option<FunId> {
    if class.is_companion || !hidden_primary_marker(ir, class) {
        return None;
    }
    class.methods.iter().copied().find(|&function| {
        ir.serialization_cache_methods.contains(&function)
            && ir.method_visibility(function) != Visibility::Private
    })
}

fn hidden_primary_marker(ir: &IrFile, class: &IrClass) -> bool {
    class.has_primary_ctor
        && marker_accessor_emitted_last(ir, class)
        && has_ctor_marker_accessor(ir, class)
}

pub(super) fn emit(
    ir: &IrFile,
    class: &IrClass,
    fq_name: &str,
    facade: &str,
    env: &EmitEnv<'_>,
    writer: &mut ClassWriter,
) {
    constructor_accessors::emit_sealed_accessors(ir, class, fq_name, writer);
    let marker_accessor = hidden_primary_marker(ir, class);
    let companion_marker = class.is_companion && marker_accessor;
    if companion_marker {
        static_accessors::emit_before_companion_marker(
            ir,
            env.run,
            &env.run.static_accessor_plan.borrow(),
            StaticOwner::Class(class.fq_name),
            facade,
            static_accessors::class_declaration_line(class),
            writer,
        );
    } else {
        static_accessors::emit(
            ir,
            env.run,
            &env.run.static_accessor_plan.borrow(),
            StaticOwner::Class(class.fq_name),
            facade,
            static_accessors::class_declaration_line(class),
            writer,
        );
    }
    // A companion's marker constructor follows property/static accessors but precedes a
    // companion-instance accessor. A class hiding a value-class primary places the marker after
    // all of its static accessors.
    if marker_accessor {
        let physical_parameters = class_ctor_jvm_tys(ir, class);
        let parameter_identities = crate::jvm::method_parameters::primary_constructor_identities(
            ir,
            class,
            &physical_parameters,
        );
        constructor_defaults::emit_ctor_marker_accessor(
            fq_name,
            &physical_parameters,
            &parameter_identities,
            writer,
        );
    }
    if let Some(accessor) = deferred_cache_accessor(ir, class)
        .filter(|&accessor| ir.functions[accessor as usize].body.is_some())
    {
        let owner = StaticOwner::Class(class.fq_name);
        emit_method(ir, accessor, owner, fq_name, facade, writer, false, env);
    }
    // kotlinc adds a value-class constructor's marker accessor after every declared member: the
    // primary's, then each secondary's in declaration order.
    if !class.is_sealed && !class.is_enum {
        for (ordinal, constructor) in class.secondary_ctors.iter().enumerate() {
            if constructor.vc_params {
                constructor_accessors::emit_secondary_marker_accessor(
                    ir, class, ordinal, fq_name, writer,
                );
            }
        }
    }
    if companion_marker {
        static_accessors::emit_after_companion_marker(
            ir,
            env.run,
            &env.run.static_accessor_plan.borrow(),
            StaticOwner::Class(class.fq_name),
            facade,
            static_accessors::class_declaration_line(class),
            writer,
        );
    }
    if class.is_companion {
        super::jvm_static_members::emit_private_bridge_accessors(
            ir,
            class,
            fq_name,
            static_accessors::class_declaration_line(class),
            writer,
        );
    }
}
