//! Value-field lowering for an atomic whose `java.util.concurrent.atomic` field updater is absent.
//!
//! On the JVM those classes exist and [`super::field_updaters`] rewrites the atomic. Native and
//! Wasm have no such classes. This pass stores the atomic's value in its backing field and
//! rewrites each operation to the corresponding read or integer update of that field: the Wasm
//! atomicfu runtime implements the same operations that way, and a single-threaded execution of
//! them observes the same results. The JVM path never reaches here.

use crate::ir::{ClassId, ExprId, IrBinOp, IrConst, IrExpr, IrFile};
use crate::types::Ty;

/// An atomic property lowered to its backing field.
pub(super) struct DirectAtomic {
    pub(super) class: ClassId,
    pub(super) field: u32,
    pub(super) ty: Ty,
    /// The constructor store of the property, and the value the `atomic(…)` call wraps.
    pub(super) initializer: (ExprId, ExprId),
}

/// One operation on a [`DirectAtomic`].
pub(super) struct DirectOperation {
    pub(super) expression: ExprId,
    pub(super) instance: ExprId,
    pub(super) arguments: Vec<ExprId>,
    pub(super) name: String,
    pub(super) class: ClassId,
    pub(super) field: u32,
    pub(super) ty: Ty,
}

/// Rewrite `atomics` and `operations` in place. Each operation's expression keeps its identity, so
/// the checked type recorded for it stays attached.
pub(super) fn rewrite(ir: &mut IrFile, atomics: &[DirectAtomic], operations: &[DirectOperation]) {
    for atomic in atomics {
        let field = &mut ir.classes[atomic.class as usize].fields[atomic.field as usize];
        field.ty = atomic.ty;
        field.flags = field.flags.with_is_final(false);
        let (statement, raw) = atomic.initializer;
        if let IrExpr::SetField { value, .. } = &mut ir.exprs[statement as usize] {
            *value = raw;
        }
    }
    let mut temps = 100_000;
    for operation in operations {
        rewrite_operation(ir, operation, &mut temps);
    }
}

fn rewrite_operation(ir: &mut IrFile, operation: &DirectOperation, temps: &mut u32) {
    match (operation.name.as_str(), operation.arguments.as_slice()) {
        ("value", []) => {
            let read = field_read(ir, operation.instance, operation);
            install(ir, operation.expression, Vec::new(), Some(read));
        }
        ("value" | "lazySet", [value]) => {
            let write = field_write(ir, operation.instance, operation, *value);
            install(ir, operation.expression, vec![write], None);
        }
        ("compareAndSet", [expected, update]) => {
            compare_and_set(ir, operation, temps, *expected, *update)
        }
        ("getAndSet", [update]) => update_field(ir, operation, temps, *update),
        ("getAndAdd", [delta]) => adjust(ir, operation, temps, IrBinOp::Add, *delta, false),
        ("addAndGet", [delta]) => adjust(ir, operation, temps, IrBinOp::Add, *delta, true),
        ("getAndIncrement", []) => {
            let step = one(ir, operation.ty);
            adjust(ir, operation, temps, IrBinOp::Add, step, false);
        }
        ("incrementAndGet", []) => {
            let step = one(ir, operation.ty);
            adjust(ir, operation, temps, IrBinOp::Add, step, true);
        }
        ("getAndDecrement", []) => {
            let step = one(ir, operation.ty);
            adjust(ir, operation, temps, IrBinOp::Sub, step, false);
        }
        ("decrementAndGet", []) => {
            let step = one(ir, operation.ty);
            adjust(ir, operation, temps, IrBinOp::Sub, step, true);
        }
        _ => {}
    }
}

/// Read the field, combine it with `delta`, and store the combination.
///
/// `return_new` is `addAndGet`: the result is the stored value. Otherwise the result is the value
/// read before the store.
fn adjust(
    ir: &mut IrFile,
    operation: &DirectOperation,
    temps: &mut u32,
    op: IrBinOp,
    delta: ExprId,
    return_new: bool,
) {
    let mut statements = Vec::new();
    let receiver = capture(ir, operation.instance, temps, &mut statements);
    let read = field_read(ir, receiver, operation);
    let current = hold(ir, read, operation.ty, temps, &mut statements);
    let updated = ir.add_arithmetic(op, current, delta, operation.ty);
    let stored = if return_new {
        hold(ir, updated, operation.ty, temps, &mut statements)
    } else {
        updated
    };
    statements.push(field_write(ir, receiver, operation, stored));
    let result = if return_new { stored } else { current };
    install(ir, operation.expression, statements, Some(result));
}

fn update_field(ir: &mut IrFile, operation: &DirectOperation, temps: &mut u32, update: ExprId) {
    let mut statements = Vec::new();
    let receiver = capture(ir, operation.instance, temps, &mut statements);
    let read = field_read(ir, receiver, operation);
    let current = hold(ir, read, operation.ty, temps, &mut statements);
    statements.push(field_write(ir, receiver, operation, update));
    install(ir, operation.expression, statements, Some(current));
}

fn compare_and_set(
    ir: &mut IrFile,
    operation: &DirectOperation,
    temps: &mut u32,
    expected: ExprId,
    update: ExprId,
) {
    let mut statements = Vec::new();
    let receiver = capture(ir, operation.instance, temps, &mut statements);
    let read = field_read(ir, receiver, operation);
    let current = hold(ir, read, operation.ty, temps, &mut statements);
    let condition = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: current,
        rhs: expected,
    });
    let write = field_write(ir, receiver, operation, update);
    let updated = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    let unchanged = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
    let then_branch = ir.add_expr(IrExpr::Block {
        stmts: vec![write],
        value: Some(updated),
    });
    ir.set_logical_type(then_branch, Ty::Boolean);
    let decision = ir.add_expr(IrExpr::When {
        branches: vec![(Some(condition), then_branch), (None, unchanged)],
    });
    ir.set_logical_type(decision, Ty::Boolean);
    install(ir, operation.expression, statements, Some(decision));
}

/// Evaluate `instance` once. A read and a write of the same operation share the result.
fn capture(
    ir: &mut IrFile,
    instance: ExprId,
    temps: &mut u32,
    statements: &mut Vec<ExprId>,
) -> ExprId {
    let Some(ty) = ir.logical_type(instance) else {
        return instance;
    };
    hold(ir, instance, ty, temps, statements)
}

fn hold(
    ir: &mut IrFile,
    value: ExprId,
    ty: Ty,
    temps: &mut u32,
    statements: &mut Vec<ExprId>,
) -> ExprId {
    let slot = *temps;
    *temps += 1;
    let declaration = ir.add_expr(IrExpr::Variable {
        index: slot,
        ty,
        init: Some(value),
        named: false,
    });
    statements.push(declaration);
    let read = ir.add_expr(IrExpr::GetValue(slot));
    ir.set_logical_type(read, ty);
    read
}

fn field_read(ir: &mut IrFile, receiver: ExprId, operation: &DirectOperation) -> ExprId {
    let read = ir.add_expr(IrExpr::GetField {
        receiver,
        class: operation.class,
        index: operation.field,
    });
    ir.set_logical_type(read, operation.ty);
    read
}

fn field_write(
    ir: &mut IrFile,
    receiver: ExprId,
    operation: &DirectOperation,
    value: ExprId,
) -> ExprId {
    ir.add_expr(IrExpr::SetField {
        receiver,
        class: operation.class,
        index: operation.field,
        value,
    })
}

fn one(ir: &mut IrFile, ty: Ty) -> ExprId {
    ir.add_expr(IrExpr::Const(if ty == Ty::Long {
        IrConst::Long(1)
    } else {
        IrConst::Int(1)
    }))
}

fn install(ir: &mut IrFile, expression: ExprId, stmts: Vec<ExprId>, value: Option<ExprId>) {
    ir.exprs[expression as usize] = IrExpr::Block { stmts, value };
}
