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
    // A lambda whose body breaks out of, or continues, a loop of its caller is legal only as an
    // argument an inline call inlines: its body has no standalone method to live in, as kotlinc
    // never writes one. A call that keeps such a lambda as a value is rescued and fails closed.
    dead.extend(ir.exprs.iter().filter_map(|expression| match expression {
        IrExpr::Lambda {
            impl_fn,
            inline_body: Some(body),
            ..
        } if !spliced_as_a_call.contains(impl_fn) && jumps_out_of_caller_loop(ir, *body) => {
            Some(*impl_fn)
        }
        _ => None,
    }));
    for fid in dead {
        ir.inline_only_fns.insert(fid);
        ir.must_inline_lambdas.insert(fid);
    }
}

/// Whether `body` holds a `break` or `continue` whose loop is not inside `body`.
fn jumps_out_of_caller_loop(ir: &IrFile, body: u32) -> bool {
    fn escapes(ir: &IrFile, expression: u32, loops: &mut Vec<Option<String>>) -> bool {
        match ir.expr(expression) {
            IrExpr::Break { label } | IrExpr::Continue { label } => match label {
                None => loops.is_empty(),
                Some(label) => !loops
                    .iter()
                    .any(|enclosing| enclosing.as_deref() == Some(label.as_str())),
            },
            IrExpr::While { label, .. } => {
                loops.push(label.clone());
                let escaped = children_escape(ir, expression, loops);
                loops.pop();
                escaped
            }
            IrExpr::Checked(crate::ir::IrCheckedOperation::RangeLoop { label, .. }) => {
                loops.push(Some(label.clone()));
                let escaped = children_escape(ir, expression, loops);
                loops.pop();
                escaped
            }
            _ => children_escape(ir, expression, loops),
        }
    }
    fn children_escape(ir: &IrFile, expression: u32, loops: &mut Vec<Option<String>>) -> bool {
        let mut escaped = false;
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
            escaped = escaped || escapes(ir, child, loops);
        });
        escaped
    }
    escapes(ir, body, &mut Vec::new())
}
