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
    let suspend_lambda_bodies = suspend_lambda_body_expressions(ir);
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
            let in_suspend_lambda = suspend_lambda_bodies.contains(&access.operation);
            mark_private_cross_class_access(
                ir,
                &layout,
                access.operation,
                access.read,
                in_suspend_lambda,
            );
        }
        mark_private_member_extension(ir, &layout, access.operation, access.read);
    }
    Ok(())
}

/// Every expression of a suspend lambda's body. The JVM backend makes such a lambda a class of its
/// own (`SuspendLambda`) after this realization runs, so its body reaches the enclosing class's
/// private members from outside even though its expressions still belong to that class here. A
/// lambda inlined into its caller instead keeps the caller's class, where the access stays direct.
fn suspend_lambda_body_expressions(ir: &IrFile) -> std::collections::HashSet<ExprId> {
    let mut expressions = std::collections::HashSet::new();
    let mut pending: Vec<ExprId> = ir
        .suspend_funs
        .iter()
        .filter(|function| ir.lambda_origins.contains_key(function))
        .filter_map(|&function| ir.functions.get(function as usize)?.body)
        .collect();
    while let Some(expression) = pending.pop() {
        if expressions.insert(expression) {
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        }
    }
    expressions
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
    in_suspend_lambda: bool,
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
    if !in_suspend_lambda && ir.expression_owners.get(&operation).copied() == Some(declaring) {
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
