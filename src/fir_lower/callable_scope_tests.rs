//! Which blocks are a callable's own scope: the block its body lowers from, and the blocks lowering
//! builds around it, but not a block nested in the body or one a class initializer runs.

use super::tests::lower_single_source;
use crate::ir::{ExprId, IrExpr, IrFile};

/// Whether each block whose statements declare the local named `name` is a callable scope, in
/// expression order.
fn declaring_blocks(ir: &IrFile, name: &str) -> Vec<bool> {
    let declares = |statement: &ExprId| {
        matches!(ir.expr(*statement), IrExpr::Variable { .. })
            && ir.value_names.get(statement).map(String::as_str) == Some(name)
    };
    (0..ir.exprs.len())
        .map(|raw| u32::try_from(raw).expect("test IR is small"))
        .filter(|&expression| {
            matches!(ir.expr(expression), IrExpr::Block { stmts, .. } if stmts.iter().any(declares))
        })
        .map(|block| ir.callable_scopes.contains(&block))
        .collect()
}

#[test]
fn a_callable_body_block_is_its_scope_and_a_nested_block_is_not() {
    let ir = lower_single_source(
        "fun sink(value: Int) {}\n\
         fun declared(flag: Boolean) {\n\
         \x20   val own = 1\n\
         \x20   if (flag) { val nested = 2; sink(nested) }\n\
         \x20   sink(own)\n\
         }\n\
         fun lambda(): () -> Unit = { val captured = 3; sink(captured) }\n\
         fun value(): () -> Int = { val held = 5; held }\n\
         class Holder { init { val initialized = 4; sink(initialized) } }\n",
        "CallableScopes",
    );
    assert_eq!(declaring_blocks(&ir, "own"), [true]);
    assert_eq!(declaring_blocks(&ir, "nested"), [false]);
    assert_eq!(declaring_blocks(&ir, "initialized"), [false]);
    // The lambda's implementation method, then the template a call site that inlines it copies:
    // an inlined copy is scoped by its call site, not by the lambda.
    assert_eq!(declaring_blocks(&ir, "captured"), [true, false]);
    // A value-returning lambda's body is a block the return wraps. That block is still the
    // callable's scope: its locals cover the `return`, not only the block.
    assert_eq!(declaring_blocks(&ir, "held"), [true, false]);
}
