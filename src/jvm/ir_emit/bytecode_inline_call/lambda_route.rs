//! Which lowering a call to a classpath inline function with literal lambda arguments takes. The
//! route is decided from checked facts before any of the call's code is emitted and is never
//! retried as the other: the MethodInliner port owns every shape it covers, and the byte splice
//! keeps, by name, the lambda and callee shapes later stages of the port take over
//! ([`SpliceReason`]).

use super::*;
use crate::jvm::inliner::UnsupportedShape;

/// The lowering of one inline call with literal lambda arguments.
pub(in crate::jvm::ir_emit) enum LambdaCallRoute {
    /// kotlinc's `MethodInliner` over the callee's node.
    MethodInliner(MethodNode),
    /// The byte splice, for a shape the port does not own yet.
    Splice(SpliceReason),
}

/// A shape the byte splice still owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::jvm::ir_emit) enum SpliceReason {
    /// A lambda suspends. The splice places its state-machine markers until the coroutine
    /// transformer runs on inlined bytecode.
    SuspendingLambda,
    /// A lambda leaves by a non-local `return`, `break` or `continue`.
    NonLocalJump,
    /// A lambda's parameter or result crosses `invoke` through a value-class adapter rather than a
    /// plain box.
    ValueClassAdapter,
    /// A literal lambda of the call is materialized as an object, which needs kotlinc's
    /// anonymous-object regeneration.
    MaterializedLambda,
    /// The callee needs a later stage of the port.
    CalleeShape(UnsupportedShape),
    /// The callee has a try/catch or a backward jump and the call starts with operands on the
    /// stack; kotlinc spills them around the inlined body, which is not ported.
    OperandsAcrossCallee,
    /// An `@InlineOnly` callee takes its arguments in place, beside a lambda.
    InPlaceArguments,
    /// A lambda captures a value that is not a caller local of its own type.
    CaptureOutsideFrame,
}

impl Emitter<'_> {
    /// The route of `call`, whose callee's body invokes at least one of the literal lambdas among
    /// its arguments. `materialized` is the call's published
    /// materialization role for each value parameter. An error is a fact the checked IR or the
    /// callee's class file should have made impossible.
    pub(in crate::jvm::ir_emit) fn lambda_call_route(
        &mut self,
        call: &ClasspathInlineCall<'_, '_>,
        materialized: &[bool],
        code: &CodeBuilder,
    ) -> Result<LambdaCallRoute, &'static str> {
        let ClasspathInlineCall {
            call_expression,
            target,
            args,
            leading_non_argument_operands,
            body,
            ..
        } = *call;
        let callee = MethodNode::read(ACC_STATIC, target.name, target.splice_desc, body)
            .map_err(|_| UNREADABLE_INLINE_BODY)?;
        // Only the MethodNode inliner specializes reified type parameters, so a reified body in a
        // shape the port does not own yet fails cleanly instead of taking the splice.
        let reified_body = inliner::has_reified_markers(&callee);
        let splice = |reason| {
            if reified_body {
                Err(REIFIED_BODY_ON_BYTE_SPLICE)
            } else {
                Ok(LambdaCallRoute::Splice(reason))
            }
        };
        let physical = parse_descriptor_params(target.splice_desc)
            .filter(|physical| physical.len() == args.len())
            .ok_or("an inline callee's descriptor does not match the call's operands")?;
        let mut lambda_arguments = Vec::new();
        for (index, &argument) in args.iter().enumerate() {
            if !matches!(
                self.ir.expr(argument),
                IrExpr::Lambda {
                    inline_body: Some(_),
                    ..
                }
            ) {
                continue;
            }
            let is_materialized = index
                .checked_sub(leading_non_argument_operands)
                .and_then(|parameter| materialized.get(parameter))
                .ok_or("a lambda argument has no published materialization role")?;
            if *is_materialized {
                return splice(SpliceReason::MaterializedLambda);
            }
            if let Some(reason) = self.lambda_splice_reason(argument) {
                return splice(reason);
            }
            lambda_arguments.push(argument);
        }
        if let Some(shape) =
            inliner::unsupported_shape(&callee, inliner::ObjectRegeneration::Declined)
        {
            return splice(SpliceReason::CalleeShape(shape));
        }
        if inliner::requires_empty_stack_on_entry(&callee) && code.stack_height() != 0 {
            return splice(SpliceReason::OperandsAcrossCallee);
        }
        let supplies = self
            .parameter_supplies(
                call_expression,
                target,
                args,
                leading_non_argument_operands,
                &physical,
                &callee,
            )
            .ok_or("a function-typed argument has no published materialization role")?;
        if supplies.contains(&Supply::InPlace) {
            return splice(SpliceReason::InPlaceArguments);
        }
        if !lambda_arguments
            .iter()
            .all(|&argument| self.lambda_captures_caller_locals(argument))
        {
            return splice(SpliceReason::CaptureOutsideFrame);
        }
        Ok(LambdaCallRoute::MethodInliner(callee))
    }
}

/// Whether `body` leaves itself by a `return` (every `return` in a lambda's inline body is
/// non-local: a local return is lowered to the body's value) or by a `break`/`continue` whose loop
/// is outside `body`.
pub(super) fn leaves_by_non_local_jump(ir: &IrFile, body: u32) -> bool {
    fn escapes(ir: &IrFile, expression: u32, loops: &mut Vec<Option<String>>) -> bool {
        match ir.expr(expression) {
            IrExpr::Return(_) => true,
            IrExpr::Break { label } | IrExpr::Continue { label } => match label {
                None => loops.is_empty(),
                Some(label) => !loops
                    .iter()
                    .any(|enclosing| enclosing.as_ref() == Some(label)),
            },
            IrExpr::While { label, .. } => {
                loops.push(label.clone());
                let mut escaped = false;
                crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                    escaped = escaped || escapes(ir, child, loops);
                });
                loops.pop();
                escaped
            }
            _ => {
                let mut escaped = false;
                crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                    escaped = escaped || escapes(ir, child, loops);
                });
                escaped
            }
        }
    }
    escapes(ir, body, &mut Vec::new())
}

#[cfg(test)]
mod tests {
    use super::leaves_by_non_local_jump;
    use crate::ir::{IrExpr, IrFile};

    fn block(ir: &mut IrFile, stmts: Vec<u32>) -> u32 {
        ir.add_expr(IrExpr::Block { stmts, value: None })
    }

    fn looping(ir: &mut IrFile, body: u32, label: Option<&str>) -> u32 {
        let cond = ir.add_expr(IrExpr::UnitInstance);
        ir.add_expr(IrExpr::While {
            cond,
            body,
            update: None,
            post_test: false,
            label: label.map(str::to_string),
        })
    }

    #[test]
    fn a_return_leaves_the_lambda() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Return(None));
        let body = block(&mut ir, vec![exit]);
        assert!(leaves_by_non_local_jump(&ir, body));
    }

    #[test]
    fn a_break_of_a_loop_inside_the_lambda_stays_in_it() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Break { label: None });
        let inner = looping(&mut ir, exit, None);
        let body = block(&mut ir, vec![inner]);
        assert!(!leaves_by_non_local_jump(&ir, body));
    }

    #[test]
    fn a_break_or_continue_of_a_loop_outside_the_lambda_leaves_it() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Break { label: None });
        let body = block(&mut ir, vec![exit]);
        assert!(leaves_by_non_local_jump(&ir, body));

        let next = ir.add_expr(IrExpr::Continue {
            label: Some("outer".to_string()),
        });
        let inner = looping(&mut ir, next, Some("inner"));
        let body = block(&mut ir, vec![inner]);
        assert!(leaves_by_non_local_jump(&ir, body));
    }
}
