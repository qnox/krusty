//! Backend-boundary realization of checked counted loops over a progression, in the shape of
//! kotlinc's `ForLoopsLowering` (`ProgressionHeaderInfo` and `ProgressionLoopHeader.buildLoop`).
//!
//! Checked FIR publishes the progression tree (a range literal, a progression value, `step`,
//! `reversed`), its lowered operands, the loop variable's identity, and bound stability in common
//! IR. A backend chooses the physical control-flow shape here, after common lowering has finished.
//! In particular, only the JVM selects the Java-like counter loop HotSpot recognizes.
//!
//! A header is built from the progression tree as kotlinc's handlers build one (`header`), and the
//! header decides the loop's shape (`loop_shape`):
//!
//! ```text
//! // the induction variable may overflow
//! if (inductionVar <= last) do { body; if (inductionVar == last) break; inductionVar += step } while (true)
//!
//! // exclusive last on a target preferring Java-like counter loops (the JVM)
//! while (inductionVar < last) { body; inductionVar += step }
//!
//! // any other bound that cannot overflow
//! if (inductionVar <= last) do { val i = inductionVar; inductionVar += step; body } while (inductionVar <= last)
//! ```
//!
//! with the comparison written `last < inductionVar` (`last <= inductionVar`) for a decreasing
//! progression, and `(step > 0 && inductionVar <= last) || (step < 0 && last <= inductionVar)`
//! when the direction is unknown.

mod header;
mod loop_shape;

use crate::ir::{
    Callee, ExprId, IrBindingStability, IrCheckedOperation, IrConst, IrExpr, IrFile,
    IrProgressionSource, IrRuntimeFunction, IrShortCircuitKind, IrTypeOp,
};
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
    let mut next_slot = None;
    for expression in 0..ir.exprs.len() {
        let IrExpr::Checked(IrCheckedOperation::RangeLoop {
            variable,
            variable_name,
            counter,
            source,
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
        let mut realizer = Realizer {
            next_slot: next_slot.unwrap_or_else(|| ir.next_value_slot()),
            ir: &mut *ir,
            style,
        };
        let replacement = realizer.progression_loop(CountedLoop {
            variable,
            variable_name,
            counter,
            source,
            body,
            label,
        });
        next_slot = Some(realizer.next_slot);
        record_generated_origins(ir, expression as ExprId, first_generated);
        ir.exprs[expression] = replacement;
    }
}

/// The checked pieces of one counted loop.
struct CountedLoop {
    variable: u32,
    variable_name: Option<Box<str>>,
    counter: Ty,
    source: IrProgressionSource,
    body: ExprId,
    label: String,
}

/// A header operand and whether its value can change while the loop runs
/// (`canChangeValueDuringExecution`), which decides whether it is copied to a temporary.
#[derive(Clone, Copy)]
struct Operand {
    value: ExprId,
    can_change: bool,
}

impl Operand {
    fn stable(value: ExprId) -> Self {
        Self {
            value,
            can_change: false,
        }
    }
}

/// The IR file being rewritten, the target's loop style, and the next free value slot.
struct Realizer<'a> {
    ir: &'a mut IrFile,
    style: CounterLoopStyle,
    next_slot: u32,
}

impl Realizer<'_> {
    fn add(&mut self, expression: IrExpr) -> ExprId {
        self.ir.add_expr(expression)
    }

    /// A second use of a leaf operand (a constant or a value read) as its own node.
    fn reread(&mut self, expression: ExprId) -> ExprId {
        let copy = self.ir.expr(expression).clone();
        self.add(copy)
    }

    fn allocate_temporary(&mut self) -> u32 {
        let slot = self.next_slot;
        self.next_slot = slot
            .checked_add(1)
            .expect("counted-loop temporary index exceeds u32");
        slot
    }

    /// `createLoopTemporaryVariableIfNecessary`: an operand that cannot change while the loop runs
    /// is re-read where it is used; anything else is copied to a temporary first.
    fn loop_temporary(
        &mut self,
        operand: Operand,
        ty: Ty,
        statements: &mut Vec<ExprId>,
    ) -> (Option<u32>, ExprId) {
        if !operand.can_change {
            return (None, operand.value);
        }
        let slot = self.allocate_temporary();
        let declaration = self.add(IrExpr::Variable {
            index: slot,
            ty,
            init: Some(operand.value),
            named: false,
        });
        statements.push(declaration);
        (Some(slot), self.add(IrExpr::GetValue(slot)))
    }

    /// The loop variable's declaration, carrying its source name.
    fn loop_variable_declaration(
        &mut self,
        variable: u32,
        name: Option<&str>,
        ty: Ty,
        initializer: ExprId,
    ) -> ExprId {
        let declaration = self.add(IrExpr::Variable {
            index: variable,
            ty,
            init: Some(initializer),
            named: true,
        });
        if let Some(name) = name {
            self.ir.value_names.insert(declaration, name.to_owned());
        }
        declaration
    }

    /// A header operand read from a checked bound: a constant or a stable read of a local of the
    /// operand's own type cannot change (a widened bound such as `0L..n` is a conversion).
    fn bound_operand(&mut self, bound: ExprId, ty: Ty) -> Operand {
        let stable = constant_bound(self.ir, bound).is_some()
            || (self.ir.binding_read_stability.get(&bound) == Some(&IrBindingStability::Stable)
                && self.ir.logical_types.get(&bound) == Some(&ty)
                && matches!(self.ir.expr(bound), IrExpr::GetValue(_)));
        Operand {
            value: self.range_bound(bound, ty),
            can_change: !stable,
        }
    }

    fn range_bound(&mut self, expression: ExprId, target: Ty) -> ExprId {
        self.add(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: expression,
            type_operand: target,
        })
    }

    /// A call of a runtime function resolution selected, realized by the backend like any other
    /// call of that external declaration.
    fn runtime_call(&mut self, function: &IrRuntimeFunction, args: Vec<ExprId>) -> ExprId {
        self.add(IrExpr::Call {
            callee: Callee::External {
                target: function.function,
                default_provider: None,
                params: function.parameters.clone(),
                ret: function.result,
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: None,
            },
            dispatch_receiver: None,
            args,
        })
    }

    fn short_circuit(&mut self, kind: IrShortCircuitKind, lhs: ExprId, rhs: ExprId) -> ExprId {
        let branches = match kind {
            IrShortCircuitKind::And => {
                let false_value = self.add(IrExpr::Const(IrConst::Boolean(false)));
                vec![(Some(lhs), rhs), (None, false_value)]
            }
            IrShortCircuitKind::Or => {
                let true_value = self.add(IrExpr::Const(IrConst::Boolean(true)));
                vec![(Some(lhs), true_value), (None, rhs)]
            }
        };
        let when = self.add(IrExpr::When { branches });
        self.ir.short_circuits.insert(when, kind);
        when
    }
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

/// The constant under an implicit numeric coercion, which is how a lowered literal bound arrives.
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

/// `constLongValue`.
fn constant_value(ir: &IrFile, expression: ExprId) -> Option<i64> {
    constant_bound(ir, expression)
        .as_ref()
        .and_then(integral_value)
}

fn step_constant(step_ty: Ty, value: i64) -> IrConst {
    if step_ty == Ty::Long {
        IrConst::Long(value)
    } else {
        IrConst::Int(value as i32)
    }
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
