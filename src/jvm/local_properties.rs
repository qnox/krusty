//! JVM bookkeeping layered on exact common property-target realization.

use crate::fir::PropertyId;
use crate::ir::{ExprId, IrFile, IrLocalPropertyLayout};
use crate::jvm::property_realizations::PropertyRealizations;
use crate::types::Ty;

pub(super) fn realize(
    ir: &mut IrFile,
    realizations: &mut PropertyRealizations,
) -> Result<(), PropertyId> {
    let accesses = crate::backend::local_properties::realize(ir)?;
    for access in accesses {
        let Some(layout) = ir.local_property_layouts.get(&access.target).cloned() else {
            return Err(access.target);
        };
        if access.read {
            // A checked use-site conversion may specialize a declaration type parameter to a value
            // class. The JVM accessor still returns the declaration's erased physical type.
            if let Some(ty) = property_declaration_type(&layout) {
                ir.property_declaration_types.insert(access.operation, ty);
                if ty.is_ty_param() {
                    ir.physical_types.insert(access.operation, ty);
                }
            }
        }
        if access.direct_member {
            realizations.record_local(access.operation, access.target);
            mark_private_cross_class_access(ir, &layout, access.operation, access.read);
        }
        mark_private_member_extension(ir, &layout, access.operation, access.read);
    }
    Ok(())
}

fn property_declaration_type(layout: &IrLocalPropertyLayout) -> Option<Ty> {
    match layout {
        IrLocalPropertyLayout::Member { ty, .. }
        | IrLocalPropertyLayout::MemberExtension { ty, .. } => Some(*ty),
        IrLocalPropertyLayout::TopLevelStorage { .. }
        | IrLocalPropertyLayout::TopLevelAccessor { .. } => None,
    }
}

/// A Kotlin-private member reached from a different source classifier needs a JVM access bridge.
/// The decision consumes the exact operation and declaration layout; it performs no member lookup.
/// A read of a property with a declared getter, and a write of one with a declared setter, reaches
/// that exact accessor. Bridge collection reads the recorded function and does not scan by name.
fn mark_private_cross_class_access(
    ir: &mut IrFile,
    layout: &IrLocalPropertyLayout,
    operation: ExprId,
    read: bool,
) {
    let IrLocalPropertyLayout::Member {
        class,
        property,
        private: true,
        ..
    } = layout
    else {
        return;
    };
    let declaring = ir.classes[*class as usize].fq_name;
    if ir.expression_owners.get(&operation).copied() == Some(declaring) {
        return;
    }
    if let Some(declaration) = ir.classes[*class as usize]
        .properties
        .get_mut(*property as usize)
    {
        declaration.needs_access_bridge = true;
        let accessor = if read {
            declaration.getter
        } else {
            declaration.setter
        };
        if let Some(function) = accessor {
            ir.jvm_member_targets.insert(operation, function);
        }
    }
}

/// A private member-extension accessor is an instance method of the declaring class. Another class
/// reaches it through `access$<name>`, the same bridge an ordinary private member uses. The
/// cross-owner walk bridges the function only when a different class calls it, so a call inside
/// the declaring class stays direct.
fn mark_private_member_extension(
    ir: &mut IrFile,
    layout: &IrLocalPropertyLayout,
    operation: ExprId,
    read: bool,
) {
    let IrLocalPropertyLayout::MemberExtension { getter, setter, .. } = layout else {
        return;
    };
    let function = if read { Some(*getter) } else { *setter };
    let Some(function) = function.filter(|&function| ir.method_visibility(function).is_private())
    else {
        return;
    };
    ir.jvm_member_targets.insert(operation, function);
}
