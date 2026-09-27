//! Lambda implementations a must-inline call splices, whose standalone method is dead.

use crate::ir::{Callee, IrExpr, IrFile};

/// Lambda impl methods that a lambda's own `inline_body` CALLS. An ANONYMOUS FUNCTION cannot be
/// spliced verbatim — its `return` is LOCAL, so a copied body would return from the enclosing method —
/// and the lowerer therefore gives it an `inline_body` that is an `invokestatic` to its impl. Such an
/// impl is LIVE even though no `invokedynamic` ever references it, and must survive both the
/// must-inline dead-marking and the facade dead-lambda sweep.
pub(super) fn splice_called_impls(ir: &IrFile) -> std::collections::HashSet<u32> {
    ir.exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn,
                inline_body: Some(body),
                ..
            } => matches!(
                &ir.exprs[*body as usize],
                IrExpr::Call { callee: Callee::Local(f), .. } if f == impl_fn
            )
            .then_some(*impl_fn),
            _ => None,
        })
        .collect()
}

pub fn mark_must_inline_lambdas(ir: &mut IrFile) {
    let spliced_as_a_call = splice_called_impls(ir);
    let mut dead: Vec<u32> = Vec::new();
    for i in 0..ir.exprs.len() {
        let args = match &ir.exprs[i] {
            IrExpr::Call {
                callee:
                    Callee::Static {
                        inline: crate::libraries::InlineKind::MustInline,
                        ..
                    },
                args,
                ..
            } => args.clone(),
            IrExpr::Call { args, .. }
                if ir
                    .module_inline_calls
                    .contains(&(u32::try_from(i).expect("IR expression index exceeds ExprId"))) =>
            {
                args.clone()
            }
            _ => continue,
        };
        for a in args {
            if let IrExpr::Lambda { impl_fn, .. } = &ir.exprs[a as usize] {
                if !spliced_as_a_call.contains(impl_fn) {
                    dead.push(*impl_fn);
                }
            }
        }
    }
    for fid in dead {
        ir.inline_only_fns.insert(fid);
        ir.must_inline_lambdas.insert(fid);
    }
}
