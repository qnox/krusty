//! JVM realization of backend-neutral checked range construction.

use std::rc::Rc;

use super::{classpath::Classpath, jvm_libraries::JvmLibraries};
use crate::fir::FirRangeOperation;
use crate::ir::{Callee, ExprId, IrCheckedOperation, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RangeRealizationFailure {
    Initialization(crate::libraries::PlatformInitializationError),
    Operation(RangeOperationFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RangeOperationFailure {
    expression: ExprId,
    operation: FirRangeOperation,
    start: Ty,
    end: Ty,
}

/// Realize the exact checked range operation selected by the frontend. The runtime provider owns
/// JVM class names, descriptors, helper facades, and synthetic constructor slots; this pass does no
/// lookup or overload selection.
pub(super) fn realize(
    ir: &mut IrFile,
    classpath: Rc<Classpath>,
) -> Result<(), RangeRealizationFailure> {
    crate::backend::counted_loops::realize(
        ir,
        crate::backend::counted_loops::CounterLoopStyle::JavaLike,
    );
    let runtime = JvmLibraries::new(classpath).map_err(RangeRealizationFailure::Initialization)?;
    let expression_count = ir.exprs.len();
    for expression in 0..expression_count {
        let IrExpr::Checked(IrCheckedOperation::RangeConstruction {
            operation,
            start,
            start_type,
            end,
            end_type,
            result,
        }) = ir.exprs[expression].clone()
        else {
            continue;
        };
        let failure = RangeRealizationFailure::Operation(RangeOperationFailure {
            expression: expression as ExprId,
            operation,
            start: start_type,
            end: end_type,
        });
        let construction = runtime
            .range_construction(start_type, end_type)
            .ok_or_else(|| failure.clone())?;
        let start = coerce(
            ir,
            expression as ExprId,
            start,
            start_type,
            construction.elem,
        );
        let end = coerce(ir, expression as ExprId, end, end_type, construction.elem);
        let physical_result = construction.result;
        let physical = match operation {
            FirRangeOperation::Through => {
                if let Some(callable) = construction.through_static {
                    static_call(callable, start, end)
                } else {
                    let mut arguments = vec![start, end];
                    for _ in 0..construction.through.trailing_nulls {
                        let null = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
                        copy_expression_facts(ir, expression as ExprId, null);
                        arguments.push(null);
                    }
                    IrExpr::New {
                        internal: crate::types::type_name(&construction.through.internal),
                        args: arguments,
                        ctor_params: None,
                        ctor_desc: Some(construction.through.ctor_desc),
                        external_target: None,
                        defaults: Box::new([]),
                        default_prefix_count: 0,
                    }
                }
            }
            FirRangeOperation::OpenEnd | FirRangeOperation::Until => static_call(
                construction.until.ok_or_else(|| failure.clone())?,
                start,
                end,
            ),
            // `downTo` is an ordinary provider-selected extension call and must not arrive as a
            // compiler-supplied range-construction operation.
            FirRangeOperation::DownTo => return Err(failure),
        };
        if physical_result == result {
            ir.exprs[expression] = physical;
        } else {
            let physical = ir.add_expr(physical);
            copy_expression_facts(ir, expression as ExprId, physical);
            ir.exprs[expression] = IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: physical,
                type_operand: result,
            };
        }
    }
    for expression in 0..expression_count {
        match ir.exprs[expression].clone() {
            IrExpr::Checked(IrCheckedOperation::RangeContains {
                operation,
                value,
                start,
                end,
                negated,
                counter,
            }) => {
                let replacement = unsigned_range_contains(
                    ir, &runtime, operation, value, start, end, negated, counter,
                )
                .ok_or(RangeRealizationFailure::Operation(
                    RangeOperationFailure {
                        expression: expression as ExprId,
                        operation,
                        start: counter,
                        end: counter,
                    },
                ))?;
                ir.exprs[expression] = replacement;
            }
            IrExpr::Checked(IrCheckedOperation::IllegalProgressionStep { step }) => {
                ir.exprs[expression] = illegal_step(ir, expression as ExprId, step);
            }
            IrExpr::Checked(
                IrCheckedOperation::Call { .. }
                | IrCheckedOperation::ConstructorDelegation { .. }
                | IrCheckedOperation::BackingFieldRead { .. }
                | IrCheckedOperation::BackingFieldWrite { .. }
                | IrCheckedOperation::LateinitFieldRead { .. }
                | IrCheckedOperation::PropertyRead { .. }
                | IrCheckedOperation::PropertyWrite { .. }
                | IrCheckedOperation::ExternalPropertyRead { .. }
                | IrCheckedOperation::ExternalPropertyWrite { .. }
                | IrCheckedOperation::PropertyReference { .. },
            )
            // The backend pass above realized every counted loop.
            | IrExpr::Checked(
                IrCheckedOperation::RangeConstruction { .. } | IrCheckedOperation::RangeLoop { .. },
            )
            | IrExpr::CallableReference(_)
            | IrExpr::Const(_)
            | IrExpr::BottomValue { .. }
            | IrExpr::ClassConst { .. }
            | IrExpr::KClassLiteral { .. }
            | IrExpr::LocalPropertyReference { .. }
            | IrExpr::SingletonValue { .. }
            | IrExpr::GetValue(_)
            | IrExpr::SetValue { .. }
            | IrExpr::Call { .. }
            | IrExpr::Return(_)
            | IrExpr::Block { .. }
            | IrExpr::When { .. }
            | IrExpr::TypeOp { .. }
            | IrExpr::While { .. }
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. }
            | IrExpr::Variable { .. }
            | IrExpr::PrimitiveBinOp { .. }
            | IrExpr::PrimitiveNeg { .. }
            | IrExpr::StringConcat(_)
            | IrExpr::PropertyRead { .. }
            | IrExpr::PropertyWrite { .. }
            | IrExpr::EnclosingInstance { .. }
            | IrExpr::GetField { .. }
            | IrExpr::LateinitInitialized { .. }
            | IrExpr::SetField { .. }
            | IrExpr::GetStatic(_)
            | IrExpr::SetStatic { .. }
            | IrExpr::New { .. }
            | IrExpr::MethodCall { .. }
            | IrExpr::EnumEntry { .. }
            | IrExpr::StaticInstance { .. }
            | IrExpr::ExternalStaticField { .. }
            | IrExpr::ExternalStaticInstance { .. }
            | IrExpr::UnitInstance
            | IrExpr::Lambda { .. }
            | IrExpr::InvokeFunction { .. }
            | IrExpr::Try { .. }
            | IrExpr::Throw { .. }
            | IrExpr::LateinitCheck { .. }
            | IrExpr::NotNullAssert { .. }
            | IrExpr::EnumValues { .. }
            | IrExpr::EnumValueOf { .. }
            | IrExpr::EnumEntries { .. }
            | IrExpr::ReifiedTypeOp { .. }
            | IrExpr::ReifiedClassMarker { .. }
            | IrExpr::RefNew { .. }
            | IrExpr::RefGet { .. }
            | IrExpr::RefSet { .. }
            | IrExpr::NewArray { .. }
            | IrExpr::Vararg { .. }
            | IrExpr::PluginPlaceholder { .. }
            | IrExpr::CurrentContinuation => {}
        }
    }
    Ok(())
}

fn unsigned_range_contains(
    ir: &mut IrFile,
    runtime: &JvmLibraries,
    operation: FirRangeOperation,
    value: ExprId,
    start: ExprId,
    end: ExprId,
    negated: bool,
    counter: Ty,
) -> Option<IrExpr> {
    let value_slot = ir.next_value_slot();
    let start_slot = value_slot.checked_add(1)?;
    let end_slot = value_slot.checked_add(2)?;
    let declarations = vec![
        ir.add_expr(IrExpr::Variable {
            index: value_slot,
            ty: counter,
            init: Some(value),
            named: false,
        }),
        ir.add_expr(IrExpr::Variable {
            index: start_slot,
            ty: counter,
            init: Some(start),
            named: false,
        }),
        ir.add_expr(IrExpr::Variable {
            index: end_slot,
            ty: counter,
            init: Some(end),
            named: false,
        }),
    ];
    let (low, high, strict_high) = match operation {
        FirRangeOperation::Through => (start_slot, end_slot, false),
        FirRangeOperation::OpenEnd | FirRangeOperation::Until => (start_slot, end_slot, true),
        FirRangeOperation::DownTo => (end_slot, start_slot, false),
    };
    let first = unsigned_compare_slots(
        ir,
        runtime,
        value_slot,
        low,
        if negated {
            crate::ir::IrBinOp::Lt
        } else {
            crate::ir::IrBinOp::Ge
        },
        counter,
    )?;
    let second = unsigned_compare_slots(
        ir,
        runtime,
        value_slot,
        high,
        match (negated, strict_high) {
            (true, true) => crate::ir::IrBinOp::Ge,
            (true, false) => crate::ir::IrBinOp::Gt,
            (false, true) => crate::ir::IrBinOp::Lt,
            (false, false) => crate::ir::IrBinOp::Le,
        },
        counter,
    )?;
    let result = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: if negated {
            crate::ir::IrBinOp::Or
        } else {
            crate::ir::IrBinOp::And
        },
        lhs: first,
        rhs: second,
    });
    Some(IrExpr::Block {
        stmts: declarations,
        value: Some(result),
    })
}

fn unsigned_compare_slots(
    ir: &mut IrFile,
    runtime: &JvmLibraries,
    lhs: u32,
    rhs: u32,
    operation: crate::ir::IrBinOp,
    counter: Ty,
) -> Option<ExprId> {
    let callable = runtime.unsigned_compare_callable(counter)?;
    let lhs = ir.add_expr(IrExpr::GetValue(lhs));
    let rhs = ir.add_expr(IrExpr::GetValue(rhs));
    let compared = ir.add_expr(static_call(callable, lhs, rhs));
    let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
    Some(ir.add_expr(IrExpr::PrimitiveBinOp {
        op: operation,
        lhs: compared,
        rhs: zero,
    }))
}

/// `throw IllegalArgumentException("Step must be positive, was: $step.")`, as the stdlib's `step`
/// checks it.
fn illegal_step(ir: &mut IrFile, cause: ExprId, step: ExprId) -> IrExpr {
    let prefix = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
        crate::kt_string::KtString::from("Step must be positive, was: "),
    )));
    let suffix = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
        crate::kt_string::KtString::from("."),
    )));
    let message = ir.add_expr(IrExpr::StringConcat(vec![prefix, step, suffix]));
    let exception = ir.add_expr(IrExpr::New {
        internal: crate::types::type_name("java/lang/IllegalArgumentException"),
        args: vec![message],
        ctor_params: None,
        ctor_desc: Some("(Ljava/lang/String;)V".to_string()),
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    for expression in [prefix, suffix, message, exception] {
        copy_expression_facts(ir, cause, expression);
    }
    IrExpr::Throw { operand: exception }
}

fn static_call(callable: crate::libraries::LibraryCallable, start: ExprId, end: ExprId) -> IrExpr {
    IrExpr::Call {
        callee: Callee::Static {
            owner: callable.owner,
            name: callable.name,
            descriptor: callable.descriptor,
            inline: callable.inline,
        },
        dispatch_receiver: None,
        args: vec![start, end],
    }
}

fn coerce(ir: &mut IrFile, cause: ExprId, value: ExprId, source: Ty, target: Ty) -> ExprId {
    if source == target {
        return value;
    }
    let value = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: value,
        type_operand: target,
    });
    copy_expression_facts(ir, cause, value);
    value
}

fn copy_expression_facts(ir: &mut IrFile, source: ExprId, target: ExprId) {
    if let Some(origin) = ir.fir_origins.get(&source).copied() {
        ir.fir_origins.insert(target, origin);
    }
    if let Some(line) = ir.expr_lines.get(&source).copied() {
        ir.expr_lines.insert(target, line);
    }
    if let Some(line) = ir.expr_source_lines.get(&source).copied() {
        ir.expr_source_lines.insert(target, line);
    }
    if let Some(line) = ir.expr_end_lines.get(&source).copied() {
        ir.expr_end_lines.insert(target, line);
    }
}
