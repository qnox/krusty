//! kotlinc's specialized `==`/`!=` over a value class (`JvmInlineClassLowering.specializeEqualsCall`).
//!
//! The left operand is of value class `V` (nullable or not), and at least one operand is carried
//! unboxed: a non-null `V`, or a `V?` over a reference carrier. The comparison calls
//! `equals-impl0(left, right)` when the right operand is an unboxed `V` too, and
//! `equals-impl(left, right)` (the right side as a reference) otherwise:
//! - a nullable left operand is null-checked first, giving `right == null` when it is null, and is
//!   unboxed when `V?` is boxed;
//! - a nullable unboxed right operand is null-checked next, giving `false` when it is null.
//!
//! An operand that is not already a variable read goes to a temporary, in source order, which the
//! bytecode optimizer folds back onto the stack as kotlinc's does. `!=` negates the same
//! expression. Two boxed operands, or a value class only on the right, keep the boxed `areEqual`.

use super::{desc, erase, Repr, ReprCtx, Under};
use crate::ir::{Callee, ExprId, IrBinOp, IrConst, IrExpr, IrFile};
use crate::libraries::InlineKind;
use crate::types::{Ty, TypeName};

/// The specialized comparison chosen for one `==`/`!=`.
#[derive(Clone, Copy)]
pub(super) struct Equality {
    value_class: TypeName,
    left: Operand,
    right: Operand,
}

/// How one operand is carried.
#[derive(Clone, Copy)]
struct Operand {
    nullable: bool,
    /// The operand is the value class's carrier, not a reference to a box or another object.
    unboxed: bool,
    /// The operand's type as a temporary holds it.
    ty: Ty,
}

/// The specialization kotlinc applies to `left == right`, if any.
pub(super) fn specialize(ctx: &ReprCtx<'_>, left: ExprId, right: ExprId) -> Option<Equality> {
    let is_null = |id: ExprId| matches!(ctx.exprs[id as usize], IrExpr::Const(IrConst::Null));
    if is_null(left) || is_null(right) {
        return None;
    }
    let carrier = |x: TypeName, nullable: bool| {
        let carrier = erase(ctx.under.get(&x)?, ctx.under);
        Some(if nullable {
            Ty::nullable(carrier)
        } else {
            carrier
        })
    };
    let left_nullable = !ctx.operand_nonnull(left);
    let (value_class, left) = match ctx.repr(left) {
        Repr::Unboxed(x) => (
            x,
            Operand {
                nullable: left_nullable,
                unboxed: true,
                ty: carrier(x, left_nullable)?,
            },
        ),
        Repr::Boxed(x) if left_nullable => (
            x,
            Operand {
                nullable: true,
                unboxed: false,
                ty: Ty::nullable(Ty::obj_name(x)),
            },
        ),
        _ => return None,
    };
    let right_nullable = !ctx.operand_nonnull(right);
    let right = match ctx.repr(right) {
        Repr::Unboxed(y) if y == value_class => Operand {
            nullable: right_nullable,
            unboxed: true,
            ty: carrier(y, right_nullable)?,
        },
        Repr::Boxed(y) if y == value_class => Operand {
            nullable: right_nullable,
            unboxed: false,
            ty: Ty::nullable(Ty::obj_name(y)),
        },
        Repr::NotVc => Operand {
            nullable: right_nullable,
            unboxed: false,
            ty: ctx
                .types
                .get(&right)
                .copied()
                .filter(|ty| ty.is_nullable() || !ty.is_jvm_scalar())?,
        },
        _ => return None,
    };
    (left.unboxed || right.unboxed).then_some(Equality {
        value_class,
        left,
        right,
    })
}

/// Rewrite the `==`/`!=` at `id` as `equality`. `fresh` is the next unused local slot.
pub(super) fn realize(
    ir: &mut IrFile,
    id: ExprId,
    equality: Equality,
    under: &Under,
    fresh: &mut u32,
) {
    let IrExpr::PrimitiveBinOp { op, lhs, rhs } = ir.exprs[id as usize] else {
        unreachable!("a specialized equality is a primitive `==`/`!=`");
    };
    let Equality {
        value_class: owner,
        left,
        right,
    } = equality;
    let carrier = under
        .get(&owner)
        .map(|underlying| erase(underlying, under))
        .expect("a specialized value class has an underlying type");
    let left_check = left.nullable;
    let right_check = right.unboxed && right.nullable;
    if !left_check && !right_check {
        let equals = compare(ir, owner, &carrier, right.unboxed, lhs, rhs);
        ir.exprs[id as usize] = negated_if(ir, equals, matches!(op, IrBinOp::Ne));
        return;
    }
    let mut stmts = Vec::new();
    let mut hold = |ir: &mut IrFile, value: ExprId, ty: Ty| match ir.exprs[value as usize] {
        IrExpr::GetValue(slot) => slot,
        _ => {
            let slot = *fresh;
            *fresh += 1;
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(value),
                named: false,
            }));
            slot
        }
    };
    let left_slot = hold(ir, lhs, left.ty);
    let right_slot = hold(ir, rhs, right.ty);
    let read = |ir: &mut IrFile, slot: u32| ir.add_expr(IrExpr::GetValue(slot));
    let mut left_value = read(ir, left_slot);
    if left_check && !left.unboxed {
        left_value = unbox(ir, left_value, owner, &carrier);
    }
    let right_value = read(ir, right_slot);
    let mut equals = compare(ir, owner, &carrier, right.unboxed, left_value, right_value);
    if right_check {
        let unequal = constant(ir, IrConst::Boolean(false));
        equals = if_null(ir, right_slot, unequal, equals);
    }
    if left_check {
        let both_null = if right.nullable {
            let right_value = read(ir, right_slot);
            let null = constant(ir, IrConst::Null);
            ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: right_value,
                rhs: null,
            })
        } else {
            constant(ir, IrConst::Boolean(false))
        };
        equals = if_null(ir, left_slot, both_null, equals);
    }
    let block = ir.add_expr(IrExpr::Block {
        stmts,
        value: Some(equals),
    });
    ir.exprs[id as usize] = negated_if(ir, block, matches!(op, IrBinOp::Ne));
}

/// `value`, or `value == false` for `!=`, which kotlinc emits as the negated branch over it.
pub(super) fn negated_if(ir: &mut IrFile, value: ExprId, negate: bool) -> IrExpr {
    if !negate {
        return IrExpr::Block {
            stmts: Vec::new(),
            value: Some(value),
        };
    }
    let zero = constant(ir, IrConst::Int(0));
    IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: value,
        rhs: zero,
    }
}

/// `equals-impl0(left, right)` over two carriers, or `equals-impl(left, right)` with a reference.
pub(super) fn compare(
    ir: &mut IrFile,
    owner: TypeName,
    carrier: &Ty,
    right_unboxed: bool,
    left: ExprId,
    right: ExprId,
) -> ExprId {
    let carrier = desc(carrier);
    let value = Ty::obj_name(owner);
    let (name, descriptor, right_parameter) = if right_unboxed {
        ("equals-impl0", format!("({carrier}{carrier})Z"), value)
    } else {
        (
            "equals-impl",
            format!("({carrier}Ljava/lang/Object;)Z"),
            Ty::nullable(Ty::obj("kotlin/Any")),
        )
    };
    let call = ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner,
            name: name.to_string(),
            descriptor,
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![left, right],
    });
    // The declared parameters: the value class, and the other operand as the class or `Any?`. An
    // `Any?` carrier erases to `Object` like the second parameter, which only this tells apart.
    ir.call_declared_params
        .insert(call, vec![value, right_parameter].into_boxed_slice());
    call
}

/// `if (slot == null) when_null else otherwise`.
fn if_null(ir: &mut IrFile, slot: u32, when_null: ExprId, otherwise: ExprId) -> ExprId {
    let value = ir.add_expr(IrExpr::GetValue(slot));
    let null = constant(ir, IrConst::Null);
    let is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: value,
        rhs: null,
    });
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(is_null), when_null), (None, otherwise)],
    })
}

fn constant(ir: &mut IrFile, value: IrConst) -> ExprId {
    ir.add_expr(IrExpr::Const(value))
}

fn unbox(ir: &mut IrFile, receiver: ExprId, owner: TypeName, carrier: &Ty) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Callee::Virtual {
            owner,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){}", desc(carrier)),
            params: None,
            interface: false,
        },
        dispatch_receiver: Some(receiver),
        args: vec![],
    })
}
