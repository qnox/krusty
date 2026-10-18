//! Whether a file's IR is ready for JVM emission: every type and callee the emitter consumes has
//! been realized, and no checked operation, plugin placeholder or local delegate access remains.

use super::{Callee, IrExpr, IrFile, Ty};

pub(crate) fn jvm_can_emit(ir: &IrFile) -> bool {
    fn ty_ok(t: &Ty) -> bool {
        match t.non_null() {
            Ty::Fun(s) => s.params.iter().all(ty_ok) && ty_ok(&s.ret),
            Ty::Obj(_, type_args) => type_args.iter().all(ty_ok),
            _ => true,
        }
    }
    fn callee_ok(callee: &Callee) -> bool {
        match callee {
            Callee::Static { .. } | Callee::Special { .. } => true,
            // Realized into `Callee::Special` before emission.
            Callee::Super { params, ret, .. } => params.iter().all(ty_ok) && ty_ok(ret),
            // A user (sibling-file) method carries `Ty`s; a classpath one a descriptor string.
            Callee::Virtual { params, .. } => match params {
                Some((ps, ret)) => ps.iter().all(ty_ok) && ty_ok(ret),
                None => true,
            },
            Callee::CrossFile { params, ret, .. } => params.iter().all(ty_ok) && ty_ok(ret),
            Callee::Module { .. }
            | Callee::ModuleWithDefaults { .. }
            | Callee::LocalWithDefaults { .. }
            | Callee::ClassStaticWithDefaults { .. }
            | Callee::External { .. } => false,
            Callee::Local(_)
            | Callee::LocalDefault(_)
            | Callee::ClassStatic { .. }
            | Callee::ClassStaticDefault { .. }
            | Callee::Intrinsic { .. } => true,
        }
    }
    if ir
        .functions
        .iter()
        .any(|f| !ty_ok(&f.ret) || !f.params.iter().all(ty_ok))
    {
        return false;
    }
    if ir.statics.iter().any(|s| !ty_ok(&s.ty)) {
        return false;
    }
    ir.exprs.iter().all(|e| match e {
        IrExpr::Lambda { .. } => true,
        IrExpr::Variable { ty, .. } => ty_ok(ty),
        IrExpr::Call { callee, .. } => callee_ok(callee),
        // Every other checked operation must have been consumed by its JVM realization pass.
        IrExpr::Checked(_) => false,
        // A plugin placeholder that reached emit means its owning plugin didn't run (or couldn't
        // specialize it) — decline the file rather than miscompile (the node has no JVM lowering).
        IrExpr::PluginPlaceholder { .. } => false,
        IrExpr::LocalDelegateAccess(_) => false,
        _ => true,
    })
}

/// Name the residual nodes that made [`jvm_can_emit`] decline the file.
pub(super) fn trace_residual_nodes(ir: &IrFile) {
    crate::trace_compiler!(
        "lower",
        "JVM emission declined residual IR nodes: {:?}",
        ir.exprs
            .iter()
            .enumerate()
            .filter_map(|(expression, value)| match value {
                IrExpr::Checked(_)
                | IrExpr::PluginPlaceholder { .. }
                | IrExpr::LocalDelegateAccess(_)
                | IrExpr::Call {
                    callee: Callee::Module { .. } | Callee::External { .. },
                    ..
                } => Some((expression, value)),
                _ => None,
            })
            .collect::<Vec<_>>()
    );
}
