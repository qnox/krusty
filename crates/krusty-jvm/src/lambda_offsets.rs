//! kotlinc's JVM `PatchLambdaOffsetsLowering`: a lambda literal takes the position of the nearest
//! call that encloses it, so stepping stops on the call, not on the line the lambda opens on.
//!
//! It is a JVM lowering and runs after the IR plugins, so a call a plugin wraps around a lambda
//! (Compose's `rememberComposableLambda`) is the nearest call. Common lowering keeps every lambda
//! at its own source position; only this pass moves it.
//!
//! The call is the nearest one on the path from the lambda up to its enclosing declaration, however
//! deep the lambda sits in its arguments (through a constructor call, a conditional or a block). An
//! array factory (`arrayOf`) is that call too: common lowering has already replaced it with the
//! vararg whose instructions take the first element's line, and the factory's own line is the
//! vararg's factory line. A local variable's initializer is a declaration of its own, and so are a
//! `when` subject and a destructuring container, which lowering stores in variables. A lambda's or
//! a local function's body is a function of its own and has no parent here. A lambda with no
//! enclosing call keeps its own position, and so do an anonymous function (`fun() { }`) and a
//! lambda converted to a functional interface, whose instance kotlinc creates at the lambda.
//!
//! A lambda's captures are the reads its creation makes, not source expressions: kotlinc
//! materializes them after this lowering, at the lambda's patched position, so they move with it.

use krusty::ir::{ExprId, IrCheckedOperation, IrExpr, IrFile};

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
            let line = enclosing_line(ir, &parents, lambda)?;
            Some((lambda, line))
        })
        .collect::<Vec<_>>();
    for (lambda, line) in lambdas {
        ir.set_source_line(lambda, line);
        let IrExpr::Lambda { captures, .. } = ir.expr(lambda) else {
            unreachable!("selected above as a lambda");
        };
        for capture in captures.clone() {
            ir.set_source_line(capture, line);
        }
    }
}

/// The parent of every expression that is another expression's direct child.
fn parents(ir: &IrFile) -> Vec<Option<ExprId>> {
    let mut parents = vec![None; ir.exprs.len()];
    for index in 0..ir.exprs.len() {
        let parent = ExprId::try_from(index).expect("an expression id fits u32");
        krusty::ir::for_each_child(&ir.exprs, parent, &mut |child| {
            parents[child as usize].get_or_insert(parent);
        });
    }
    parents
}

/// The line of the nearest call above `lambda`, unless a declaration comes first.
fn enclosing_line(ir: &IrFile, parents: &[Option<ExprId>], lambda: ExprId) -> Option<u32> {
    let mut current = parents[lambda as usize]?;
    loop {
        if matches!(ir.expr(current), IrExpr::Variable { .. }) {
            return None;
        }
        // The factory call's line. The vararg's source line is its first element's, which is
        // where the array instructions are marked; a lambda among the elements still takes the
        // call. A vararg argument has no factory line of its own and is not a call: the lambda
        // looks on to the call the argument belongs to.
        if matches!(ir.expr(current), IrExpr::Vararg { .. }) {
            match ir.factory_line(current) {
                Some(line) => return Some(line),
                None => current = parents[current as usize]?,
            }
            continue;
        }
        if is_call(ir.expr(current)) {
            return ir.source_line(current);
        }
        current = parents[current as usize]?;
    }
}

/// Whether kotlinc realizes `expression` as an `IrCall`: a function, accessor, operator or
/// function-value invocation. A constructor call (`IrConstructorCall`, a delegating one included)
/// is not: a lambda among its arguments looks past it. An array factory is a vararg by the time
/// this pass runs; [`enclosing_line`] reads the call line it kept.
fn is_call(expression: &IrExpr) -> bool {
    match expression {
        IrExpr::Checked(operation) => matches!(&**operation, IrCheckedOperation::Call { .. }),
        IrExpr::Call { .. }
        | IrExpr::MethodCall { .. }
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
