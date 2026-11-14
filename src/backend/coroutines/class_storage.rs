//! The common-IR operations a continuation object realizes with its own fields.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{
    for_each_child, Callee, ClassId, ExprId, IrCheckedOperation, IrExpr, IrField, IrFile,
    IrPropertyDispatch,
};
use crate::types::{continuation_passing, wk, Ty};

use super::label_reentry::{MachineStorage, ReentryDecline};
use super::suspension_points::{recorded_suspension_result, suspend_call_fid};

#[cfg(test)]
mod tests;

/// Put the value namespace of one suspend function in the generated machine's fields and rewrite
/// its body to use those fields. A resume re-enters the body in a new activation, so even a value
/// declared before the suspension must be read from the same machine field afterward.
///
/// Catch parameters and lambda implementation frames have their own binding rules. A suspension
/// inside a `try` is declined by the shared splitter; catch parameters therefore remain ordinary
/// locals, and a lambda's captures are rewritten in this frame while its body is not.
pub(super) struct FrameValues {
    pub(super) slots: HashMap<u32, u32>,
    original_expressions: Vec<ExprId>,
}

/// Reserve frame fields from the source body's original value namespace without changing its
/// statement forms. The splitter needs to see `Variable`/`SetValue` leaves before they become
/// field stores.
pub(super) fn plan_frame_values(
    ir: &mut IrFile,
    function: u32,
    body: ExprId,
    class: ClassId,
) -> FrameValues {
    let mut expressions = Vec::new();
    collect_frame_expressions(ir, body, &mut HashSet::default(), &mut expressions);
    let catch_values = expressions
        .iter()
        .filter_map(|&expression| match ir.expr(expression) {
            IrExpr::Try { catches, .. } => Some(catches.iter().map(|catch| catch.var)),
            _ => None,
        })
        .flatten()
        .collect::<HashSet<_>>();
    let mut value_types = super::value_namespace::function_value_types(ir, function, body)
        .into_iter()
        .filter(|(value, _)| !catch_values.contains(value))
        .collect::<Vec<_>>();
    value_types.sort_unstable_by_key(|(value, _)| *value);

    let mut shared_cell_elements = HashMap::default();
    let declaration = &ir.functions[function as usize];
    let receiver_offset =
        u32::from(declaration.dispatch_receiver.is_some() && !declaration.is_static);
    shared_cell_elements.extend(ir.shared_capture_parameters.iter().filter_map(
        |(&(owner, parameter), &element)| {
            (owner == function)
                .then(|| {
                    receiver_offset
                        .checked_add(parameter)
                        .map(|slot| (slot, element))
                })
                .flatten()
        },
    ));
    for &expression in &expressions {
        let IrExpr::Variable {
            index,
            init: Some(initializer),
            ..
        } = ir.expr(expression)
        else {
            continue;
        };
        if let IrExpr::RefNew { elem, .. } = ir.expr(*initializer) {
            shared_cell_elements.insert(*index, *elem);
        }
    }

    let mut slots = HashMap::default();
    for (value, ty) in value_types {
        let field = u32::try_from(ir.classes[class as usize].fields.len())
            .expect("a continuation frame has fewer than u32::MAX fields");
        let field_ty = if let Some(&element) = shared_cell_elements.get(&value) {
            ir.shared_class_capture_fields
                .insert((class, field), element);
            element
        } else if ty == Ty::Unit {
            Ty::obj_name(wk::unit())
        } else {
            crate::types::stored_value_ty(ty)
        };
        ir.classes[class as usize]
            .fields
            .push(IrField::new(format!("$frame${value}"), field_ty));
        slots.insert(value, field);
    }

    FrameValues {
        slots,
        original_expressions: expressions,
    }
}

/// Rewrite only the source expression IDs collected before splitting. Newly generated machine
/// receivers and temporaries share the same arena but belong to a different value namespace.
pub(super) fn realize_frame_values(
    ir: &mut IrFile,
    frame: &FrameValues,
    class: ClassId,
    owner_ty: Ty,
) {
    for &expression in &frame.original_expressions {
        match ir.expr(expression).clone() {
            IrExpr::CurrentContinuation => {
                let Some(ty) = ir.checked_type(expression) else {
                    continue;
                };
                let current = ir.add_expr(IrExpr::GetValue(0));
                ir.set_logical_type(current, ty);
                ir.exprs[expression as usize] = ir.expr(current).clone();
            }
            IrExpr::GetValue(value) => {
                let Some(&index) = frame.slots.get(&value) else {
                    continue;
                };
                let receiver = ir.add_expr(IrExpr::GetValue(0));
                ir.set_logical_type(receiver, owner_ty);
                ir.exprs[expression as usize] = IrExpr::GetField {
                    receiver,
                    class,
                    index,
                };
            }
            IrExpr::SetValue { var, value } => {
                let Some(&index) = frame.slots.get(&var) else {
                    continue;
                };
                let receiver = ir.add_expr(IrExpr::GetValue(0));
                ir.set_logical_type(receiver, owner_ty);
                let store = ir.add_expr(IrExpr::SetField {
                    receiver,
                    class,
                    index,
                    value,
                });
                ir.exprs[expression as usize] = IrExpr::Block {
                    stmts: vec![store],
                    value: None,
                };
                ir.set_logical_type(expression, Ty::Unit);
            }
            IrExpr::Variable {
                index: value, init, ..
            } => {
                let Some(&index) = frame.slots.get(&value) else {
                    continue;
                };
                let stmts = init
                    .map(|value| {
                        let receiver = ir.add_expr(IrExpr::GetValue(0));
                        ir.set_logical_type(receiver, owner_ty);
                        vec![ir.add_expr(IrExpr::SetField {
                            receiver,
                            class,
                            index,
                            value,
                        })]
                    })
                    .unwrap_or_default();
                ir.exprs[expression as usize] = IrExpr::Block { stmts, value: None };
                ir.set_logical_type(expression, Ty::Unit);
            }
            _ => {}
        }
    }
}

fn collect_frame_expressions(
    ir: &IrFile,
    expression: ExprId,
    seen: &mut HashSet<ExprId>,
    out: &mut Vec<ExprId>,
) {
    if !seen.insert(expression) {
        return;
    }
    out.push(expression);
    if let IrExpr::Lambda { captures, .. } = ir.expr(expression) {
        for &capture in captures {
            collect_frame_expressions(ir, capture, seen, out);
        }
        return;
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        collect_frame_expressions(ir, child, seen, out);
    }
}

/// Field coordinates owned by a generated continuation class.
pub(super) struct ContinuationFields {
    pub(super) class: crate::ir::ClassId,
    pub(super) machine_ty: Ty,
    pub(super) label: u32,
    pub(super) result: u32,
    pub(super) runtime: crate::ir::IrCoroutineRuntime,
    pub(super) suspend_functions: HashSet<u32>,
}

impl ContinuationFields {
    fn this(&self, ir: &mut IrFile) -> ExprId {
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(receiver, self.machine_ty);
        receiver
    }

    fn read(&self, ir: &mut IrFile, field: u32) -> ExprId {
        let receiver = self.this(ir);
        ir.add_expr(IrExpr::GetField {
            receiver,
            class: self.class,
            index: field,
        })
    }

    fn write(&self, ir: &mut IrFile, field: u32, value: ExprId) -> ExprId {
        let receiver = self.this(ir);
        ir.add_expr(IrExpr::SetField {
            receiver,
            class: self.class,
            index: field,
            value,
        })
    }
}

impl MachineStorage for ContinuationFields {
    fn read_label(&self, ir: &mut IrFile) -> ExprId {
        self.read(ir, self.label)
    }

    fn write_label(&self, ir: &mut IrFile, label: ExprId) -> ExprId {
        self.write(ir, self.label, label)
    }

    fn resumed_value(&self, ir: &mut IrFile) -> ExprId {
        let result = self.read(ir, self.result);
        let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let result_ty = Ty::obj_args_name(crate::types::type_name("kotlin/Result"), &[any]);
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::External {
                target: self.runtime.get_or_throw,
                default_provider: None,
                params: vec![result_ty],
                ret: any,
                substitutions: vec![external_type_substitution(self.runtime.get_or_throw, any)],
                defaults: Vec::new(),
                // `getOrThrow` is a top-level extension on `Result<T>`.  Its receiver is
                // represented as argument zero in an external dependency call.
                extension_receiver_parameter: Some(0),
            }),
            dispatch_receiver: None,
            args: vec![result],
        });
        ir.call_declared_ret.insert(call, any);
        call
    }

    fn suspended_marker(&self, ir: &mut IrFile) -> ExprId {
        let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::ExternalPropertyRead {
                target: self.runtime.suspended,
                dispatch: IrPropertyDispatch::Ordinary,
                receiver: None,
                arguments: Vec::new(),
                parameters: Vec::new(),
                result: any,
                source_receiver: None,
            },
        )))
    }

    fn call_with_continuation(
        &self,
        ir: &mut IrFile,
        call: ExprId,
    ) -> Result<ExprId, ReentryDecline> {
        if ir.intrinsic_suspension_points.contains_key(&call) {
            // The splitter replaces the original suspension expression with the value supplied
            // by the resumed frame. Keep a separate root for the intrinsic invocation while
            // retaining its source operands, which frame rewriting still updates in place.
            let copy = crate::ir::relocate_with_operands(ir, call, |operand| operand);
            let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
            ir.take_physical_type(copy);
            ir.set_logical_type(copy, any);
            return Ok(copy);
        }
        let Some(logical_result) =
            recorded_suspension_result(ir, call).or_else(|| ir.checked_type(call))
        else {
            return Err(ReentryDecline::UnsupportedCallShape);
        };
        let source_declaration = if ir.suspend_calls.contains_key(&call) {
            match ir.expr(call) {
                IrExpr::Call { callee, .. } if matches!(&**callee, Callee::External { .. }) => {
                    Some((**callee).clone())
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(source_declaration) = source_declaration {
            // Keep the exact checked source application beside this expression as its physical
            // call is rewritten below. Dependency authentication uses this selected declaration
            // and substitution frame; target emission separately checks the appended continuation
            // against the exported CPS signature.
            ir.suspend_call_declarations
                .insert(call, source_declaration);
        }
        let continuation_ty = continuation_passing(logical_result).0;
        let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let completion = self.this(ir);
        ir.set_logical_type(completion, continuation_ty);
        let call = match ir.expr(call).clone() {
            IrExpr::Call { .. } => {
                let copied = crate::ir::relocate_with_operands(ir, call, |operand| operand);
                let IrExpr::Call { callee, args, .. } = &mut ir.exprs[copied as usize] else {
                    unreachable!("a copied call retains its node kind")
                };
                match &mut **callee {
                    Callee::Local(function) | Callee::ClassStatic { function, .. }
                        if self.suspend_functions.contains(function) => {}
                    Callee::External { params, ret, .. } => {
                        params.push(continuation_ty);
                        *ret = any;
                    }
                    Callee::Module { params, ret, .. } | Callee::CrossFile { params, ret, .. } => {
                        params.push(continuation_ty);
                        *ret = any;
                    }
                    _ => return Err(ReentryDecline::UnsupportedCallShape),
                }
                args.push(completion);
                copied
            }
            IrExpr::MethodCall { .. }
                if suspend_call_fid(ir, call, &self.suspend_functions).is_some() =>
            {
                let copied = crate::ir::relocate_with_operands(ir, call, |operand| operand);
                let IrExpr::MethodCall { args, .. } = &mut ir.exprs[copied as usize] else {
                    unreachable!("a copied method call retains its node kind")
                };
                args.push(Some(completion));
                copied
            }
            IrExpr::InvokeFunction { args, params, .. }
                if ir.suspend_calls.contains_key(&call) && args.len() == params.len() =>
            {
                let copied = crate::ir::relocate_with_operands(ir, call, |operand| operand);
                let IrExpr::InvokeFunction {
                    args, params, ret, ..
                } = &mut ir.exprs[copied as usize]
                else {
                    unreachable!("a copied function invocation retains its node kind")
                };
                args.push(completion);
                params.push(continuation_ty);
                *ret = any;
                copied
            }
            _ => return Err(ReentryDecline::UnsupportedCallShape),
        };
        ir.call_declared_ret.insert(call, any);
        ir.take_physical_type(call);
        ir.set_logical_type(call, any);
        Ok(call)
    }
}

pub(super) fn external_type_substitution(
    target: crate::fir::ExternalCallableId,
    value: Ty,
) -> crate::ir::IrCheckedSubstitution {
    crate::ir::IrCheckedSubstitution {
        parameter: crate::fir::FirTypeParameterRef::External {
            callable: target,
            ordinal: 0,
        },
        reified: false,
        value,
        reified_runtime: value,
        additional_bounds: Vec::new(),
    }
}
