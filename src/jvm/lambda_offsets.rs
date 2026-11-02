//! kotlinc's JVM `PatchLambdaOffsetsLowering`: a lambda literal takes the position of the nearest
//! call that encloses it, so stepping stops on the call, not on the line the lambda opens on.
//!
//! It is a JVM lowering and runs after the IR plugins, so a call a plugin wraps around a lambda
//! (Compose's `rememberComposableLambda`) is the nearest call. Common lowering keeps every lambda
//! at its own source position; only this pass moves it.
//!
//! The call is the nearest one on the path from the lambda up to its enclosing declaration, however
//! deep the lambda sits in its arguments (through a constructor call, a conditional or a block). A
//! local variable's initializer is a declaration of its own, and so are a `when` subject and a
//! destructuring container, which lowering stores in variables. A lambda's or a local function's
//! body is a function of its own and has no parent here. A lambda with no enclosing call keeps its
//! own position, and so do an anonymous function (`fun() { }`) and a lambda converted to a
//! functional interface, whose instance kotlinc creates at the lambda.

use crate::ir::{ExprId, IrCheckedOperation, IrExpr, IrFile};

/// Move each lambda literal to the line of the call nearest above it.
pub(crate) fn patch(ir: &mut IrFile) {
    let parents = parents(ir);
    let lambdas = (0..ir.exprs.len())
        .filter_map(|index| {
            let lambda = ExprId::try_from(index).expect("an expression id fits u32");
            let IrExpr::Lambda {
                impl_fn, sam: None, ..
            } = ir.expr(lambda)
            else {
                return None;
            };
            if !ir.is_lambda_literal(*impl_fn) {
                return None;
            }
            let line = ir.source_line(enclosing_call(ir, &parents, lambda)?)?;
            Some((lambda, line))
        })
        .collect::<Vec<_>>();
    for (lambda, line) in lambdas {
        ir.set_source_line(lambda, line);
    }
}

/// The parent of every expression that is another expression's direct child.
fn parents(ir: &IrFile) -> Vec<Option<ExprId>> {
    let mut parents = vec![None; ir.exprs.len()];
    for index in 0..ir.exprs.len() {
        let parent = ExprId::try_from(index).expect("an expression id fits u32");
        crate::ir::for_each_child(&ir.exprs, parent, &mut |child| {
            parents[child as usize].get_or_insert(parent);
        });
    }
    parents
}

/// The nearest call above `lambda`, unless a declaration comes first.
fn enclosing_call(ir: &IrFile, parents: &[Option<ExprId>], lambda: ExprId) -> Option<ExprId> {
    let mut current = parents[lambda as usize]?;
    loop {
        match ir.expr(current) {
            IrExpr::Variable { .. } => return None,
            _ if is_call(ir.expr(current)) => return Some(current),
            _ => current = parents[current as usize]?,
        }
    }
}

/// Whether kotlinc realizes `expression` as a function access (`IrCall`, `IrConstructorCall`):
/// a function, accessor, operator, constructor or function-value invocation.
fn is_call(expression: &IrExpr) -> bool {
    match expression {
        IrExpr::Checked(operation) => matches!(
            &**operation,
            IrCheckedOperation::Call { .. } | IrCheckedOperation::ConstructorDelegation { .. }
        ),
        IrExpr::Call { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::New { .. }
        | IrExpr::InvokeFunction { .. }
        | IrExpr::PropertyRead { .. }
        | IrExpr::PropertyWrite { .. }
        | IrExpr::PrimitiveBinOp { .. }
        | IrExpr::PrimitiveNeg { .. }
        | IrExpr::Equality { .. }
        | IrExpr::NewArray { .. }
        | IrExpr::EnumValueOf { .. } => true,
        _ => false,
    }
}
