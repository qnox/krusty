//! JVM realization of backend-neutral checked range construction.

use std::rc::Rc;

use super::{classpath::Classpath, jvm_libraries::JvmLibraries};
use crate::fir::{FirRangeOperation, SyntheticOriginKind};
use crate::ir::{Callee, ExprId, IrCheckedOperation, IrExpr, IrFile, IrNodeOrigin, IrTypeOp};
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
    crate::backend::counted_loops::realize(ir, COUNTED_LOOPS);
    let runtime = JvmLibraries::new(classpath).map_err(RangeRealizationFailure::Initialization)?;
    let expression_count = ir.exprs.len();
    for expression in 0..expression_count {
        let IrExpr::Checked(checked_op) = ir.exprs[expression].clone() else {
            continue;
        };
        let IrCheckedOperation::RangeConstruction {
            operation,
            start,
            start_type,
            end,
            end_type,
            result,
        } = *checked_op
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
            IrExpr::Checked(checked_op) => match *checked_op {
                IrCheckedOperation::IllegalProgressionStep { step } => {
                    ir.exprs[expression] = illegal_step(ir, expression as ExprId, step);
                }
                IrCheckedOperation::Call { .. }
                | IrCheckedOperation::ConstructorDelegation { .. }
                | IrCheckedOperation::BackingFieldRead { .. }
                | IrCheckedOperation::BackingFieldWrite { .. }
                | IrCheckedOperation::LateinitFieldRead { .. }
                | IrCheckedOperation::PropertyRead { .. }
                | IrCheckedOperation::PropertyWrite { .. }
                | IrCheckedOperation::ExternalPropertyRead { .. }
                | IrCheckedOperation::ExternalPropertyWrite { .. }
                | IrCheckedOperation::PropertyReference { .. }
                // The backend pass above realized every counted loop and range-membership test.
                | IrCheckedOperation::RangeConstruction { .. }
                | IrCheckedOperation::RangeLoop { .. }
                | IrCheckedOperation::RangeContains { .. }
                | IrCheckedOperation::ComparableRangeContains { .. } => {}
            },
            IrExpr::CallableReference(_)
            | IrExpr::Const(_)
            | IrExpr::BottomValue { .. }
            | IrExpr::ClassConst { .. }
            | IrExpr::KClassLiteral { .. }
            | IrExpr::LocalPropertyReference(_)
            | IrExpr::LocalDelegateAccess(_)
            | IrExpr::SingletonValue { .. }
            | IrExpr::GetValue(_)
            | IrExpr::SetValue { .. }
            | IrExpr::SetFrameResult { .. }
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
            | IrExpr::Equality { .. }
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
            | IrExpr::EnumSwitchOrdinal { .. }
            | IrExpr::EnumSwitchKey { .. }
            | IrExpr::ReifiedTypeOp { .. }
            | IrExpr::ReifiedClassMarker { .. }
            | IrExpr::RefNew { .. }
            | IrExpr::RefGet { .. }
            | IrExpr::RefSet { .. }
            | IrExpr::NewArray { .. }
            | IrExpr::Vararg { .. }
            | IrExpr::PluginPlaceholder { .. }
            | IrExpr::CurrentContinuation
            | IrExpr::AbsentArgument { .. }
            | IrExpr::InlineFrameMarker
            | IrExpr::ForwardedSuperArgument { .. } => {}
        }
    }
    Ok(())
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
    let physical_name = callable.physical_name().to_string();
    IrExpr::Call {
        callee: Box::new(Callee::Static {
            owner: callable.owner,
            name: physical_name,
            descriptor: callable.descriptor,
            inline: callable.inline,
        }),
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

/// Give a part of the realized construction (a coerced bound, a trailing `null`, the physical call
/// under a result coercion) the construction's source facts. Only the construction itself ends an
/// inlined call, so a part takes the cause of that provenance as its own origin.
fn copy_expression_facts(ir: &mut IrFile, source: ExprId, target: ExprId) {
    if let Some(origin) = ir.fir_origins.get(&source).copied() {
        let origin = match origin {
            IrNodeOrigin::Synthetic {
                cause,
                kind: SyntheticOriginKind::InlinedCall,
            } => IrNodeOrigin::Fir(cause),
            origin => origin,
        };
        ir.fir_origins.insert(target, origin);
    }
    if let Some(line) = ir.expr_lines.get(&source).copied() {
        ir.expr_lines.insert(target, line);
    }
    ir.copy_position(source, target);
}

/// The JVM's counted loops: Java-like counter loops HotSpot recognizes, with the calls kotlinc
/// inlines into an unsigned loop's header realized as inlined, as its line rules need.
pub(crate) const COUNTED_LOOPS: crate::backend::counted_loops::CountedLoopPolicy =
    crate::backend::counted_loops::CountedLoopPolicy {
        style: crate::backend::counted_loops::CounterLoopStyle::JavaLike,
        inlining: crate::backend::counted_loops::HeaderInlining::Kotlinc,
        until_steps: crate::backend::counted_loops::UntilSteps::Built,
    };
