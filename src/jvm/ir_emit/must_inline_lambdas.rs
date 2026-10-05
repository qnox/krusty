//! Lambda implementations a must-inline call splices, whose standalone method is dead.

use crate::ir::{Callee, ExprId, IrExpr, IrFile};

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

/// A lambda argument of an inline call that published no parameter modifier and declared type for
/// its operand. Without both facts the backend cannot know whether the call inlines the lambda, so
/// it neither marks the lambda's method dead nor places its body: the IR is invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingInlineParameterFacts {
    pub call: ExprId,
    pub parameter: usize,
}

/// Mark the standalone method of every lambda a must-inline call inlines as dead. Fails on a lambda
/// argument whose parameter facts the call did not publish: a missing fact never grants inlining.
pub fn mark_must_inline_lambdas(ir: &mut IrFile) -> Result<(), MissingInlineParameterFacts> {
    let spliced_as_a_call = splice_called_impls(ir);
    let mut dead: Vec<u32> = Vec::new();
    for i in 0..ir.exprs.len() {
        let call = u32::try_from(i).expect("IR expression index exceeds ExprId");
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
            IrExpr::Call { args, .. } if ir.module_inline_calls.contains(&call) => args.clone(),
            _ => continue,
        };
        for (parameter, a) in args.into_iter().enumerate() {
            let IrExpr::Lambda { impl_fn, .. } = &ir.exprs[a as usize] else {
                continue;
            };
            // A literal for a parameter the call publishes as no inline parameter is a value the
            // body receives, so its method is live.
            match super::inline_parameters::is_inline_parameter(ir, call, parameter) {
                Some(true) => {}
                Some(false) => continue,
                None => return Err(MissingInlineParameterFacts { call, parameter }),
            }
            if !spliced_as_a_call.contains(impl_fn) {
                dead.push(*impl_fn);
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
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrFunction;
    use crate::types::{InlineParameterModifier, Ty};

    /// A module inline call whose only operand is a literal lambda, and the lambda's method.
    fn inline_call_with_literal(ir: &mut IrFile) -> (ExprId, crate::ir::FunId) {
        let impl_fn = ir.add_fun(IrFunction {
            name: "caller$lambda".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        });
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(body),
        });
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(impl_fn),
            dispatch_receiver: None,
            args: vec![lambda],
        });
        ir.module_inline_calls.insert(call);
        (call, impl_fn)
    }

    fn assert_rejected_and_live(ir: &mut IrFile, call: ExprId, impl_fn: crate::ir::FunId) {
        assert_eq!(
            mark_must_inline_lambdas(ir),
            Err(MissingInlineParameterFacts { call, parameter: 0 })
        );
        assert!(!ir.inline_only_fns.contains(&impl_fn));
        assert!(!ir.must_inline_lambdas.contains(&impl_fn));
    }

    #[test]
    fn published_inline_parameter_facts_mark_the_literal_dead() {
        let mut ir = IrFile::default();
        let (call, impl_fn) = inline_call_with_literal(&mut ir);
        ir.call_inline_modifiers
            .insert(call, Box::new([InlineParameterModifier::None]));
        ir.call_declared_params
            .insert(call, Box::new([Ty::fun(Vec::new(), Ty::Unit)]));
        assert_eq!(mark_must_inline_lambdas(&mut ir), Ok(()));
        assert!(ir.inline_only_fns.contains(&impl_fn));
        assert!(ir.must_inline_lambdas.contains(&impl_fn));
    }

    #[test]
    fn missing_inline_parameter_facts_never_mark_a_literal_dead() {
        let mut ir = IrFile::default();
        let (call, impl_fn) = inline_call_with_literal(&mut ir);
        assert_rejected_and_live(&mut ir, call, impl_fn);
    }

    #[test]
    fn a_missing_declared_parameter_type_never_marks_a_literal_dead() {
        let mut ir = IrFile::default();
        let (call, impl_fn) = inline_call_with_literal(&mut ir);
        ir.call_inline_modifiers
            .insert(call, Box::new([InlineParameterModifier::None]));
        assert_rejected_and_live(&mut ir, call, impl_fn);
    }

    #[test]
    fn mis_sized_inline_parameter_facts_never_mark_a_literal_dead() {
        let mut ir = IrFile::default();
        let (call, impl_fn) = inline_call_with_literal(&mut ir);
        ir.call_inline_modifiers.insert(call, Box::new([]));
        ir.call_declared_params
            .insert(call, Box::new([Ty::fun(Vec::new(), Ty::Unit)]));
        assert_rejected_and_live(&mut ir, call, impl_fn);
        ir.call_inline_modifiers
            .insert(call, Box::new([InlineParameterModifier::None]));
        ir.call_declared_params.insert(call, Box::new([]));
        assert_rejected_and_live(&mut ir, call, impl_fn);
    }
}
