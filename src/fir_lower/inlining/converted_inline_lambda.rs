//! An inline lambda passed through a function-value conversion.
//!
//! `inline fun bar(f: () -> String) = foo(f)` converts `f` when `foo` expects a suspend
//! function. The conversion is a callable reference stored for the parameter, so the
//! invocation reads that local rather than the lambda the reference is bound to. A
//! non-local return makes the lambda inline-only: leaving the reference in place calls a
//! method that is never emitted. When every use of the converted local is an invocation
//! the lambda can be spliced into, those invocations are retargeted at the lambda and the
//! carrier local is dropped.

use std::collections::{HashMap, HashSet};

use crate::ir::{ExprId, IrCallableReferenceTarget, IrExpr};
use crate::types::Ty;

use super::BodyLowering;

impl BodyLowering<'_> {
    /// Retarget invocations of a function-value conversion so the ordinary lambda splice can
    /// see the inline lambda the conversion is bound to. Returns the invocations whose
    /// converted result is `Unit` and must discard the spliced value afterwards.
    pub(super) fn expose_inline_lambdas_behind_function_value_conversions(
        &mut self,
        copies: &[(ExprId, ExprId)],
    ) -> Vec<ExprId> {
        let converted = converted_slots(self.ir, copies);
        if converted.is_empty() {
            return Vec::new();
        }
        let spliceable = spliceable_invocations(self.ir, copies, &converted);
        let blocked = blocked_slots(self.ir, copies, &converted, &spliceable);
        let mut unit_results = Vec::new();
        let mut drop_variables = Vec::new();
        for (slot, carrier) in &converted {
            if blocked.contains(slot) {
                continue;
            }
            let invocations = spliceable
                .iter()
                .filter(|invoke| invoke.slot == *slot)
                .map(|invoke| invoke.invocation)
                .collect::<Vec<_>>();
            if invocations.is_empty() {
                continue;
            }
            for invocation in invocations {
                let IrExpr::InvokeFunction { func, .. } = &mut self.ir.exprs[invocation as usize]
                else {
                    continue;
                };
                *func = carrier.lambda;
                if carrier.result == Ty::Unit {
                    unit_results.push(invocation);
                }
            }
            drop_variables.push(carrier.variable);
        }
        for &variable in &drop_variables {
            for &(_, copy) in copies {
                let IrExpr::Block { stmts, .. } = &mut self.ir.exprs[copy as usize] else {
                    continue;
                };
                stmts.retain(|statement| *statement != variable);
            }
        }
        unit_results
    }

    /// The conversion adapter evaluates the function and then yields `Unit`. A spliced lambda
    /// would otherwise leave its own result in the invocation's place.
    pub(super) fn discard_converted_lambda_result(&mut self, invocation: ExprId) {
        let IrExpr::Block { stmts, value } = self.ir.expr(invocation).clone() else {
            return;
        };
        let mut stmts = stmts;
        if let Some(value) = value {
            stmts.push(value);
        }
        let unit = self.ir.add_expr(IrExpr::UnitInstance);
        self.ir.exprs[invocation as usize] = IrExpr::Block {
            stmts,
            value: Some(unit),
        };
    }
}

struct ConvertedCarrier {
    variable: ExprId,
    lambda: ExprId,
    result: Ty,
}

struct SpliceableInvoke {
    invocation: ExprId,
    func: ExprId,
    slot: u32,
}

fn converted_slots(
    ir: &crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
) -> HashMap<u32, ConvertedCarrier> {
    let mut converted = HashMap::new();
    for &(_, copy) in copies {
        let IrExpr::Variable {
            index,
            init: Some(init),
            ..
        } = ir.expr(copy)
        else {
            continue;
        };
        let Some((lambda, result)) = inline_lambda_conversion(ir, *init) else {
            continue;
        };
        converted.insert(
            *index,
            ConvertedCarrier {
                variable: copy,
                lambda,
                result,
            },
        );
    }
    converted
}

fn inline_lambda_conversion(ir: &crate::ir::IrFile, expression: ExprId) -> Option<(ExprId, Ty)> {
    let IrExpr::CallableReference(reference) = ir.expr(expression) else {
        return None;
    };
    if !matches!(
        reference.target,
        IrCallableReferenceTarget::FunctionValueConversion { .. }
    ) {
        return None;
    }
    let receiver = reference.bound_receiver?;
    if !matches!(
        ir.expr(receiver),
        IrExpr::Lambda {
            inline_body: Some(_),
            ..
        }
    ) {
        return None;
    }
    Some((receiver, reference.declaration_result))
}

fn invoke_can_splice_lambda(ir: &crate::ir::IrFile, invocation: ExprId, lambda: ExprId) -> bool {
    let IrExpr::InvokeFunction { args, .. } = ir.expr(invocation) else {
        return false;
    };
    let IrExpr::Lambda {
        impl_fn,
        arity,
        captures,
        inline_body: Some(_),
        ..
    } = ir.expr(lambda)
    else {
        return false;
    };
    let suspend = ir.suspend_funs.contains(impl_fn);
    if args.len() + usize::from(suspend) != *arity as usize {
        return false;
    }
    ir.functions
        .get(*impl_fn as usize)
        .is_some_and(|function| function.params.len() == captures.len() + args.len())
}

fn spliceable_invocations(
    ir: &crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
    converted: &HashMap<u32, ConvertedCarrier>,
) -> Vec<SpliceableInvoke> {
    let mut invocations = Vec::new();
    for &(_, copy) in copies {
        let IrExpr::InvokeFunction { func, .. } = ir.expr(copy) else {
            continue;
        };
        let IrExpr::GetValue(slot) = ir.expr(*func) else {
            continue;
        };
        let Some(carrier) = converted.get(slot) else {
            continue;
        };
        if !invoke_can_splice_lambda(ir, copy, carrier.lambda) {
            continue;
        }
        invocations.push(SpliceableInvoke {
            invocation: copy,
            func: *func,
            slot: *slot,
        });
    }
    invocations
}

fn blocked_slots(
    ir: &crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
    converted: &HashMap<u32, ConvertedCarrier>,
    spliceable: &[SpliceableInvoke],
) -> HashSet<u32> {
    let spliceable_funcs = spliceable
        .iter()
        .map(|invoke| invoke.func)
        .collect::<HashSet<_>>();
    let mut blocked = HashSet::new();
    for &(_, copy) in copies {
        let slot = match ir.expr(copy) {
            IrExpr::GetValue(slot) | IrExpr::SetValue { var: slot, .. } => *slot,
            _ => continue,
        };
        if !converted.contains_key(&slot) || spliceable_funcs.contains(&copy) {
            continue;
        }
        blocked.insert(slot);
    }
    for (&slot, carrier) in converted {
        let quoted = copies.iter().any(|&(_, copy)| {
            matches!(
                ir.expr(copy),
                IrExpr::Block {
                    value: Some(value),
                    ..
                } if *value == carrier.variable
            )
        });
        if quoted {
            blocked.insert(slot);
        }
    }
    blocked
}

#[cfg(test)]
mod tests {
    use crate::ir::{IrCallableReferenceTarget, IrConst, IrExpr};

    fn function_body(ir: &crate::ir::IrFile, name: &str) -> crate::ir::ExprId {
        ir.functions
            .iter()
            .find(|function| function.name == name)
            .and_then(|function| function.body)
            .unwrap_or_else(|| panic!("{name} body"))
    }

    fn subtree_contains(
        ir: &crate::ir::IrFile,
        root: crate::ir::ExprId,
        predicate: &impl Fn(&IrExpr) -> bool,
    ) -> bool {
        fn visit(
            ir: &crate::ir::IrFile,
            expression: crate::ir::ExprId,
            predicate: &impl Fn(&IrExpr) -> bool,
            seen: &mut std::collections::HashSet<crate::ir::ExprId>,
        ) -> bool {
            if !seen.insert(expression) {
                return false;
            }
            if predicate(ir.expr(expression)) {
                return true;
            }
            let mut found = false;
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                if !found && visit(ir, child, predicate, seen) {
                    found = true;
                }
            });
            found
        }
        visit(ir, root, predicate, &mut std::collections::HashSet::new())
    }

    fn lower(source: &str, stem: &str) -> crate::ir::IrFile {
        crate::fir_lower::tests::lower_single_source(source, stem)
    }

    #[test]
    fn a_non_local_return_is_spliced_through_a_suspend_conversion() {
        let ir = lower(
            "suspend inline fun foo(f: suspend () -> String) = f()\n\
             suspend inline fun bar(f: () -> String) = foo(f)\n\
             suspend fun test(): String {\n\
                 bar { return \"OK\" }\n\
                 return \"Fail\"\n\
             }\n",
            "SuspendConversionNonLocal",
        );
        let test = function_body(&ir, "test");
        assert!(
            !subtree_contains(&ir, test, &|expression| matches!(
                expression,
                IrExpr::Lambda { .. }
            )),
            "the inline lambda must be spliced out of test"
        );
        assert!(
            !subtree_contains(&ir, test, &|expression| {
                matches!(
                    expression,
                    IrExpr::CallableReference(reference)
                        if matches!(
                            reference.target,
                            IrCallableReferenceTarget::FunctionValueConversion { .. }
                        )
                )
            }),
            "the suspend conversion carrier must not be evaluated"
        );
        assert!(
            subtree_contains(&ir, test, &|expression| {
                matches!(
                    expression,
                    IrExpr::Const(IrConst::String(text)) if text.as_str() == Some("OK")
                )
            }),
            "the non-local return value must be in test"
        );
    }

    #[test]
    fn an_inline_lambda_value_is_spliced_through_a_suspend_conversion() {
        let ir = lower(
            "suspend inline fun foo(f: suspend () -> String) = f()\n\
             suspend inline fun bar(f: () -> String) = foo(f)\n\
             suspend fun test(): String = bar { \"OK\" }\n",
            "SuspendConversionValue",
        );
        let test = function_body(&ir, "test");
        assert!(!subtree_contains(&ir, test, &|expression| matches!(
            expression,
            IrExpr::Lambda { .. }
        )));
        assert!(subtree_contains(&ir, test, &|expression| {
            matches!(
                expression,
                IrExpr::Const(IrConst::String(text)) if text.as_str() == Some("OK")
            )
        }));
    }

    #[test]
    fn a_function_value_keeps_its_suspend_conversion() {
        let ir = lower(
            "suspend inline fun foo(f: suspend () -> String) = f()\n\
             suspend fun test(g: () -> String): String = foo(g)\n",
            "SuspendConversionValueKeepsCarrier",
        );
        let test = function_body(&ir, "test");
        assert!(subtree_contains(&ir, test, &|expression| {
            matches!(
                expression,
                IrExpr::CallableReference(reference)
                    if matches!(
                        reference.target,
                        IrCallableReferenceTarget::FunctionValueConversion { .. }
                    )
            )
        }));
    }
}
