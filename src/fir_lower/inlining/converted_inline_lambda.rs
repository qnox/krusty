//! An inline lambda hidden from the splicer by a local.
//!
//! Two carriers put the lambda in a local and invoke that local. A function-value
//! conversion stores a callable reference for the parameter. An external inline such as
//! `run` copies a non-shared capture into an unnamed temporary; same-file expansion then
//! substitutes an inline lambda into that temporary's initializer. Either way the
//! invocation reads the local, so the direct-lambda splice never sees the lambda. A
//! non-local return makes the lambda inline-only: leaving the local in place calls a
//! method that is never emitted. When every use of the local is an invocation the lambda
//! can be spliced into, those invocations are retargeted at the lambda and the carrier
//! is dropped. A nested inline may copy that carrier into another unnamed temporary
//! first; that copy is the same lambda. A named source binding stays a value: kotlinc
//! rejects `val y = x` on an inline parameter.

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
        drop_variable_statements(self.ir, copies, &drop_variables);
        unit_results
    }

    /// Retarget invocations of an unnamed temporary whose initializer is an inline lambda
    /// substituted for an inline parameter. The temporary is the capture copy an external
    /// inline left behind; dropping it lets the ordinary lambda splice consume the invocation.
    ///
    /// A nested inline copies that temporary into another unnamed temporary before invoking it.
    /// Both copies are the same lambda: the inner invoke is retargeted, and each pure forward
    /// is dropped with the temporary it copied.
    pub(super) fn expose_inline_lambdas_behind_capture_copies(
        &mut self,
        copies: &[(ExprId, ExprId)],
        substituted_lambdas: &HashSet<ExprId>,
    ) {
        let mut copied = copied_lambda_slots(self.ir, copies, substituted_lambdas);
        if copied.is_empty() {
            return;
        }
        include_forwarded_lambda_slots(self.ir, copies, &mut copied);
        let spliceable = spliceable_invocations(self.ir, copies, &copied);
        let blocked = blocked_slots(self.ir, copies, &copied, &spliceable);
        let mut dropping = HashSet::new();
        for (slot, carrier) in &copied {
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
            }
            dropping.insert(*slot);
        }
        // A temporary that only initializes another dropped copy is the same dead carrier.
        loop {
            let mut progressed = false;
            for &(_, copy) in copies {
                let IrExpr::Variable {
                    index,
                    init: Some(init),
                    named: false,
                    ..
                } = self.ir.expr(copy)
                else {
                    continue;
                };
                if !dropping.contains(index) {
                    continue;
                }
                let IrExpr::GetValue(source) = self.ir.expr(*init) else {
                    continue;
                };
                if copied.contains_key(source)
                    && !blocked.contains(source)
                    && dropping.insert(*source)
                {
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        let drop_variables = copied
            .iter()
            .filter(|(slot, _)| dropping.contains(slot))
            .map(|(_, carrier)| carrier.variable)
            .collect::<Vec<_>>();
        drop_variable_statements(self.ir, copies, &drop_variables);
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

fn drop_variable_statements(
    ir: &mut crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
    variables: &[ExprId],
) {
    for &variable in variables {
        for &(_, copy) in copies {
            let IrExpr::Block { stmts, .. } = &mut ir.exprs[copy as usize] else {
                continue;
            };
            stmts.retain(|statement| *statement != variable);
        }
    }
}

fn copied_lambda_slots(
    ir: &crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
    substituted_lambdas: &HashSet<ExprId>,
) -> HashMap<u32, ConvertedCarrier> {
    let mut copied = HashMap::new();
    for &(_, copy) in copies {
        let IrExpr::Variable {
            index,
            init: Some(init),
            named: false,
            ..
        } = ir.expr(copy)
        else {
            continue;
        };
        if !substituted_lambdas.contains(init) {
            continue;
        }
        if !matches!(
            ir.expr(*init),
            IrExpr::Lambda {
                inline_body: Some(_),
                ..
            }
        ) {
            continue;
        }
        copied.insert(
            *index,
            ConvertedCarrier {
                variable: copy,
                lambda: *init,
                result: Ty::Unit,
            },
        );
    }
    copied
}

/// An unnamed temporary initialized by reading another carrier slot is the same lambda.
/// Nested `inline` calls copy a capture once per level (`use { use { shuffle(paths) } }`).
fn include_forwarded_lambda_slots(
    ir: &crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
    slots: &mut HashMap<u32, ConvertedCarrier>,
) {
    loop {
        let mut added = false;
        for &(_, copy) in copies {
            let IrExpr::Variable {
                index,
                init: Some(init),
                named: false,
                ..
            } = ir.expr(copy)
            else {
                continue;
            };
            if slots.contains_key(index) {
                continue;
            }
            let IrExpr::GetValue(source) = ir.expr(*init) else {
                continue;
            };
            let Some(carrier) = slots.get(source) else {
                continue;
            };
            let lambda = carrier.lambda;
            let result = carrier.result;
            slots.insert(
                *index,
                ConvertedCarrier {
                    variable: copy,
                    lambda,
                    result,
                },
            );
            added = true;
        }
        if !added {
            break;
        }
    }
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
        if !super::lambda_invocation_is_spliceable(ir, copy, carrier.lambda) {
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
    let spliceable_edges = spliceable
        .iter()
        .map(|invoke| (invoke.func, invoke.invocation))
        .collect::<HashSet<_>>();
    let mut parents = HashMap::<ExprId, Vec<ExprId>>::new();
    for &(_, parent) in copies {
        crate::ir::for_each_child(&ir.exprs, parent, &mut |child| {
            parents.entry(child).or_default().push(parent);
        });
    }
    let forward_init = |read: ExprId, parent: ExprId| {
        matches!(
            ir.expr(parent),
            IrExpr::Variable {
                index,
                init: Some(init),
                named: false,
                ..
            } if init == read && converted.contains_key(&index)
        )
    };
    let mut blocked = HashSet::new();
    for &(_, copy) in copies {
        let (slot, read) = match ir.expr(copy) {
            IrExpr::GetValue(slot) => (*slot, true),
            IrExpr::SetValue { var: slot, .. } => (*slot, false),
            _ => continue,
        };
        if !converted.contains_key(&slot) {
            continue;
        }
        let accounted = read
            && parents.get(&copy).is_some_and(|uses| {
                !uses.is_empty()
                    && uses.iter().all(|parent| {
                        spliceable_edges.contains(&(copy, *parent)) || forward_init(copy, *parent)
                    })
            });
        if !accounted {
            blocked.insert(slot);
        }
    }
    // A copy into a blocked slot is a real use of the source, not a forward the splice consumes.
    loop {
        let mut grew = false;
        for &(_, copy) in copies {
            let IrExpr::GetValue(source) = ir.expr(copy) else {
                continue;
            };
            if !converted.contains_key(&source) || blocked.contains(&source) {
                continue;
            }
            let Some(uses) = parents.get(&copy) else {
                continue;
            };
            let feeds_blocked = uses.iter().any(|parent| {
                matches!(
                    ir.expr(*parent),
                    IrExpr::Variable { index, init: Some(init), named: false, .. }
                        if init == copy && blocked.contains(&index)
                )
            });
            if feeds_blocked && blocked.insert(source) {
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    blocked
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::ir::{IrCallableReferenceTarget, IrConst, IrExpr};

    use super::{blocked_slots, copied_lambda_slots, ConvertedCarrier, SpliceableInvoke};

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
    fn only_an_unnamed_temporary_of_a_substituted_lambda_is_a_capture_copy() {
        let mut ir = crate::ir::IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(body),
        });
        let function_ty = crate::types::Ty::fun(Vec::new(), crate::types::Ty::Unit);
        let temporary = ir.add_expr(IrExpr::Variable {
            index: 3,
            ty: function_ty,
            init: Some(lambda),
            named: false,
        });
        let named = ir.add_expr(IrExpr::Variable {
            index: 4,
            ty: function_ty,
            init: Some(lambda),
            named: true,
        });
        let copies = [(0, temporary), (0, named)];
        let substituted = std::collections::HashSet::from([lambda]);

        let slots = copied_lambda_slots(&ir, &copies, &substituted);

        let mut keys = slots.keys().copied().collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(keys, vec![3]);
        assert_eq!(slots[&3].lambda, lambda);
    }

    #[test]
    fn a_shared_function_read_is_blocked_when_another_parent_keeps_it_live() {
        let mut ir = crate::ir::IrFile::default();
        let variable = ir.add_expr(IrExpr::Variable {
            index: 7,
            ty: crate::types::Ty::fun(Vec::new(), crate::types::Ty::String),
            init: None,
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(7));
        let invocation = ir.add_expr(IrExpr::InvokeFunction {
            func: read,
            args: Vec::new(),
            params: Vec::new(),
            ret: crate::types::Ty::String,
        });
        let root = ir.add_expr(IrExpr::Block {
            stmts: vec![variable, invocation],
            value: Some(read),
        });
        let converted = HashMap::from([(
            7,
            ConvertedCarrier {
                variable,
                lambda: 0,
                result: crate::types::Ty::String,
            },
        )]);
        let spliceable = [SpliceableInvoke {
            invocation,
            func: read,
            slot: 7,
        }];
        let copies = (0..=root).map(|id| (id, id)).collect::<Vec<_>>();

        assert_eq!(
            blocked_slots(&ir, &copies, &converted, &spliceable),
            std::collections::HashSet::from([7])
        );
    }

    #[test]
    fn a_capture_copied_into_another_temporary_stays_spliceable() {
        let mut ir = crate::ir::IrFile::default();
        let function_ty = crate::types::Ty::fun(Vec::new(), crate::types::Ty::Unit);
        let carrier = ir.add_expr(IrExpr::Variable {
            index: 3,
            ty: function_ty,
            init: None,
            named: false,
        });
        let forwarded_read = ir.add_expr(IrExpr::GetValue(3));
        let forwarded = ir.add_expr(IrExpr::Variable {
            index: 8,
            ty: function_ty,
            init: Some(forwarded_read),
            named: false,
        });
        let invoke_read = ir.add_expr(IrExpr::GetValue(8));
        let invocation = ir.add_expr(IrExpr::InvokeFunction {
            func: invoke_read,
            args: Vec::new(),
            params: Vec::new(),
            ret: crate::types::Ty::Unit,
        });
        let root = ir.add_expr(IrExpr::Block {
            stmts: vec![carrier, forwarded, invocation],
            value: None,
        });
        let copies = (0..=root).map(|id| (id, id)).collect::<Vec<_>>();
        let mut slots = HashMap::from([(
            3,
            ConvertedCarrier {
                variable: carrier,
                lambda: 1,
                result: crate::types::Ty::Unit,
            },
        )]);
        super::include_forwarded_lambda_slots(&ir, &copies, &mut slots);
        assert!(slots.contains_key(&8), "the inner copy is the same lambda");
        let spliceable = [SpliceableInvoke {
            invocation,
            func: invoke_read,
            slot: 8,
        }];
        assert!(
            blocked_slots(&ir, &copies, &slots, &spliceable).is_empty(),
            "a pure forward of a capture copy is still the spliced lambda"
        );
    }

    #[test]
    fn a_blocked_inner_copy_keeps_the_source_capture() {
        let mut ir = crate::ir::IrFile::default();
        let function_ty = crate::types::Ty::fun(Vec::new(), crate::types::Ty::Unit);
        let carrier = ir.add_expr(IrExpr::Variable {
            index: 3,
            ty: function_ty,
            init: None,
            named: false,
        });
        let forwarded_read = ir.add_expr(IrExpr::GetValue(3));
        let forwarded = ir.add_expr(IrExpr::Variable {
            index: 8,
            ty: function_ty,
            init: Some(forwarded_read),
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(8));
        let root = ir.add_expr(IrExpr::Block {
            stmts: vec![carrier, forwarded],
            value: Some(read),
        });
        let copies = (0..=root).map(|id| (id, id)).collect::<Vec<_>>();
        let converted = HashMap::from([
            (
                3,
                ConvertedCarrier {
                    variable: carrier,
                    lambda: 1,
                    result: crate::types::Ty::Unit,
                },
            ),
            (
                8,
                ConvertedCarrier {
                    variable: forwarded,
                    lambda: 1,
                    result: crate::types::Ty::Unit,
                },
            ),
        ]);
        assert_eq!(
            blocked_slots(&ir, &copies, &converted, &[]),
            std::collections::HashSet::from([3, 8])
        );
    }

    #[test]
    fn a_lambda_invoked_inside_nested_inline_uses_is_spliced() {
        let ir = crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(
            "import java.io.OutputStream\n\
             import java.util.zip.ZipOutputStream\n\
             inline fun zip(out: OutputStream, shuffle: (MutableList<String>) -> Unit) {\n\
                 out.use { outputStream ->\n\
                     ZipOutputStream(outputStream).use {\n\
                         shuffle(mutableListOf())\n\
                     }\n\
                 }\n\
             }\n\
             fun go(out: OutputStream) = zip(out) {}\n",
            "NestedUseLambda",
        );
        let go = function_body(&ir, "go");
        assert!(
            !subtree_contains(&ir, go, &|expression| matches!(
                expression,
                IrExpr::Lambda { .. }
            )),
            "the lambda passed through nested uses must be spliced"
        );
        assert!(
            !subtree_contains(&ir, go, &|expression| {
                matches!(expression, IrExpr::InvokeFunction { .. })
            }),
            "a spliced lambda is not invoked as a function object"
        );
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
