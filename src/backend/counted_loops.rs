//! Backend-boundary realization of checked loops over range literals.
//!
//! Checked FIR publishes the range operation, operands, binding identity, and bound stability in
//! common IR. A backend chooses the physical control-flow shape here, after common lowering has
//! finished. In particular, only the JVM selects the Java-like counter loop HotSpot recognizes.

use crate::fir::FirRangeOperation;
use crate::ir::{ExprId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CounterLoopStyle {
    /// Keep the entry comparison at the top and the overflow guard in the update.
    PreTested,
    /// Use a top-tested Java counter loop where the induction variable cannot overflow.
    JavaLike,
}

/// Realize every signed/character checked range loop. Unsigned comparisons require target runtime
/// support and deliberately remain checked for the owning backend to consume.
pub(crate) fn realize(ir: &mut IrFile, style: CounterLoopStyle) {
    let expression_count = ir.exprs.len();
    for expression in 0..expression_count {
        let IrExpr::Checked(IrCheckedOperation::RangeLoop {
            variable,
            variable_name,
            counter,
            operation,
            start,
            end,
            body,
            label,
        }) = ir.exprs[expression].clone()
        else {
            continue;
        };
        if matches!(counter, Ty::UInt | Ty::ULong) {
            continue;
        }
        let first_generated = ir.exprs.len();
        let replacement = counted_loop(
            ir,
            CountedLoop {
                style,
                variable,
                variable_name,
                counter,
                operation,
                start,
                end,
                body,
                label,
            },
        );
        record_generated_origins(ir, expression as ExprId, first_generated);
        ir.exprs[expression] = replacement;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Increasing,
    Decreasing,
}

struct ProgressionHeader {
    direction: Direction,
    last: ExprId,
    last_is_inclusive: bool,
}

struct CountedLoop {
    style: CounterLoopStyle,
    variable: u32,
    variable_name: Option<Box<str>>,
    counter: Ty,
    operation: FirRangeOperation,
    start: ExprId,
    end: ExprId,
    body: ExprId,
    label: String,
}

impl ProgressionHeader {
    fn new(
        ir: &mut IrFile,
        style: CounterLoopStyle,
        operation: FirRangeOperation,
        last: ExprId,
        ty: Ty,
    ) -> Self {
        let direction = if operation == FirRangeOperation::DownTo {
            Direction::Decreasing
        } else {
            Direction::Increasing
        };
        let inclusive = matches!(
            operation,
            FirRangeOperation::Through | FirRangeOperation::DownTo
        );
        if inclusive && style == CounterLoopStyle::JavaLike {
            if let Some(exclusive) = exclusive_bound(ir, last, direction, ty) {
                return Self {
                    direction,
                    last: exclusive,
                    last_is_inclusive: false,
                };
            }
        }
        Self {
            direction,
            last,
            last_is_inclusive: inclusive,
        }
    }

    fn condition(&self, ir: &mut IrFile, induction: u32, last: ExprId) -> ExprId {
        let induction = ir.add_expr(IrExpr::GetValue(induction));
        let op = if self.last_is_inclusive {
            IrBinOp::Le
        } else {
            IrBinOp::Lt
        };
        let (lhs, rhs) = match self.direction {
            Direction::Increasing => (induction, last),
            Direction::Decreasing => (last, induction),
        };
        ir.add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs })
    }

    fn holds_between(&self, first: &IrConst, last: &IrConst) -> Option<bool> {
        let (first, last) = (integral_value(first)?, integral_value(last)?);
        let (lower, upper) = match self.direction {
            Direction::Increasing => (first, last),
            Direction::Decreasing => (last, first),
        };
        Some(if self.last_is_inclusive {
            lower <= upper
        } else {
            lower < upper
        })
    }
}

fn counted_loop(ir: &mut IrFile, lp: CountedLoop) -> IrExpr {
    let CountedLoop {
        style,
        variable,
        variable_name,
        counter,
        operation,
        start,
        end,
        body,
        label,
    } = lp;
    let first = constant_bound(ir, start);
    let start = range_bound(ir, start, counter);
    let variable_declaration = ir.add_expr(IrExpr::Variable {
        index: variable,
        ty: counter,
        init: Some(start),
        named: true,
    });
    if let Some(name) = variable_name {
        ir.value_names.insert(variable_declaration, name.into());
    }

    let end_is_constant = constant_bound(ir, end).is_some();
    let end_is_stable_read = ir.binding_read_stability.get(&end)
        == Some(&crate::ir::IrBindingStability::Stable)
        && ir.logical_types.get(&end) == Some(&counter)
        && matches!(ir.expr(end), IrExpr::GetValue(_));
    let end = range_bound(ir, end, counter);
    let header = ProgressionHeader::new(ir, style, operation, end, counter);
    let (last_declaration, last) = if end_is_constant || end_is_stable_read {
        (None, header.last)
    } else {
        let slot = next_value_index(ir);
        let declaration = ir.add_expr(IrExpr::Variable {
            index: slot,
            ty: counter,
            init: Some(header.last),
            named: false,
        });
        (Some(declaration), ir.add_expr(IrExpr::GetValue(slot)))
    };

    let step = step_induction_variable(ir, variable, header.direction, counter);
    let loop_expression = if header.last_is_inclusive {
        let current = ir.add_expr(IrExpr::GetValue(variable));
        let at_last = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: current,
            rhs: last,
        });
        let leave = ir.add_expr(IrExpr::Break {
            label: Some(label.clone()),
        });
        let guard = ir.add_expr(IrExpr::When {
            branches: vec![(Some(at_last), leave)],
        });
        let update = ir.add_expr(IrExpr::Block {
            stmts: vec![guard, step],
            value: None,
        });
        if style == CounterLoopStyle::PreTested {
            let condition = header.condition(ir, variable, last);
            ir.add_expr(IrExpr::While {
                cond: condition,
                body,
                update: Some(update),
                post_test: false,
                label: Some(label),
            })
        } else {
            let always = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            let repeat = ir.add_expr(IrExpr::While {
                cond: always,
                body,
                update: Some(update),
                post_test: true,
                label: Some(label.clone()),
            });
            let last_constant = constant_bound(ir, last).filter(|_| counter != Ty::Long);
            if let (Some(first), Some(last_constant)) = (&first, &last_constant) {
                if header.holds_between(first, last_constant) == Some(true) {
                    repeat
                } else {
                    guarded(ir, &header, variable, last, repeat)
                }
            } else {
                guarded(ir, &header, variable, last, repeat)
            }
        }
    } else {
        let condition = header.condition(ir, variable, last);
        ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: Some(step),
            post_test: false,
            label: Some(label),
        })
    };

    let mut statements = vec![variable_declaration];
    statements.extend(last_declaration);
    statements.push(loop_expression);
    IrExpr::Block {
        stmts: statements,
        value: None,
    }
}

fn guarded(
    ir: &mut IrFile,
    header: &ProgressionHeader,
    induction: u32,
    last: ExprId,
    repeat: ExprId,
) -> ExprId {
    let entry = header.condition(ir, induction, last);
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(entry), repeat)],
    })
}

fn step_induction_variable(
    ir: &mut IrFile,
    induction: u32,
    direction: Direction,
    ty: Ty,
) -> ExprId {
    let current = ir.add_expr(IrExpr::GetValue(induction));
    let step = match direction {
        Direction::Increasing => 1,
        Direction::Decreasing => -1,
    };
    let step = ir.add_expr(IrExpr::Const(if ty == Ty::Long {
        IrConst::Long(step)
    } else {
        IrConst::Int(step as i32)
    }));
    let stepped = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Add,
        lhs: current,
        rhs: step,
    });
    let stepped = if ty == Ty::Char {
        range_bound(ir, stepped, ty)
    } else {
        stepped
    };
    ir.add_expr(IrExpr::SetValue {
        var: induction,
        value: stepped,
    })
}

fn exclusive_bound(ir: &mut IrFile, last: ExprId, direction: Direction, ty: Ty) -> Option<ExprId> {
    let delta = match direction {
        Direction::Increasing => 1,
        Direction::Decreasing => -1,
    };
    let moved = match constant_bound(ir, last)? {
        IrConst::Byte(value) => IrConst::Byte(value.checked_add(i8::try_from(delta).ok()?)?),
        IrConst::Short(value) => IrConst::Short(value.checked_add(i16::try_from(delta).ok()?)?),
        IrConst::Int(value) => IrConst::Int(value.checked_add(i32::try_from(delta).ok()?)?),
        IrConst::Long(value) => IrConst::Long(value.checked_add(delta)?),
        IrConst::Char(value) => IrConst::Char(u16::try_from(i64::from(value) + delta).ok()?),
        _ => return None,
    };
    let moved = ir.add_expr(IrExpr::Const(moved));
    Some(range_bound(ir, moved, ty))
}

fn integral_value(constant: &IrConst) -> Option<i64> {
    match *constant {
        IrConst::Byte(value) => Some(i64::from(value)),
        IrConst::Short(value) => Some(i64::from(value)),
        IrConst::Int(value) => Some(i64::from(value)),
        IrConst::Long(value) => Some(value),
        IrConst::Char(value) => Some(i64::from(value)),
        _ => None,
    }
}

fn constant_bound(ir: &IrFile, expression: ExprId) -> Option<IrConst> {
    match ir.expr(expression) {
        IrExpr::Const(constant) => Some(constant.clone()),
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => constant_bound(ir, *arg),
        _ => None,
    }
}

fn range_bound(ir: &mut IrFile, expression: ExprId, target: Ty) -> ExprId {
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: expression,
        type_operand: target,
    })
}

fn next_value_index(ir: &IrFile) -> u32 {
    ir.exprs.iter().fold(0, |next, expression| {
        let used = match expression {
            IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. }
            | IrExpr::Variable { index, .. } => Some(
                index
                    .checked_add(1)
                    .expect("common-IR value index exceeds u32"),
            ),
            IrExpr::Try { catches, .. } => catches
                .iter()
                .map(|catch| {
                    catch
                        .var
                        .checked_add(1)
                        .expect("common-IR catch value index exceeds u32")
                })
                .max(),
            _ => None,
        };
        used.map_or(next, |used| next.max(used))
    })
}

fn record_generated_origins(ir: &mut IrFile, source: ExprId, first_generated: usize) {
    let Some(origin) = ir.fir_origins.get(&source).copied() else {
        return;
    };
    let cause = match origin {
        crate::ir::IrNodeOrigin::Fir(cause) | crate::ir::IrNodeOrigin::Synthetic { cause, .. } => {
            cause
        }
    };
    for raw in first_generated..ir.exprs.len() {
        ir.fir_origins.insert(
            raw as ExprId,
            crate::ir::IrNodeOrigin::Synthetic {
                cause,
                kind: crate::fir::SyntheticOriginKind::GeneratedControlFlow,
            },
        );
    }
}
