use super::*;

pub(super) fn expression_subtree_contains(
    ir: &IrFile,
    root: crate::ir::ExprId,
    predicate: &impl Fn(crate::ir::ExprId, &IrExpr) -> bool,
) -> bool {
    fn visit(
        ir: &IrFile,
        expression: crate::ir::ExprId,
        predicate: &impl Fn(crate::ir::ExprId, &IrExpr) -> bool,
        seen: &mut crate::fx_hash::HashSet<crate::ir::ExprId>,
    ) -> bool {
        if !seen.insert(expression) {
            return false;
        }
        if predicate(expression, ir.expr(expression)) {
            return true;
        }
        let mut found = false;
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
            if !found && visit(ir, child, predicate, seen) {
                found = true;
            }
        });
        found
    }

    visit(ir, root, predicate, &mut crate::fx_hash::HashSet::default())
}

#[test]
fn lambda_callable_and_inline_template_own_distinct_return_control_flow() {
    let ir = lower_single_source(
        "fun make(): () -> Int = fun(): Int { return 1 }\n",
        "DistinctLambdaReturnBodies",
    );
    let (implementation, inline) = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn,
                inline_body: Some(inline),
                ..
            } => Some((*impl_fn, *inline)),
            _ => None,
        })
        .expect("anonymous function value");
    let callable = ir.functions[implementation as usize]
        .body
        .expect("materialized callable body");

    assert!(expression_subtree_contains(
        &ir,
        callable,
        &|_, expression| { matches!(expression, IrExpr::Return(Some(_))) }
    ));
    assert!(!expression_subtree_contains(
        &ir,
        inline,
        &|expression, node| {
            matches!(node, IrExpr::Return(_)) || ir.checked_return_depths.contains_key(&expression)
        }
    ));
    assert!(expression_subtree_contains(
        &ir,
        inline,
        &|_, expression| { matches!(expression, IrExpr::While { .. }) }
    ));
}

#[test]
fn inline_expansion_inside_lambda_consumes_its_return_depth_once() {
    let ir = lower_single_source(
        "inline fun apply(value: Int, block: (Int) -> Int): Int { return block(value) }\n\
         fun make(): () -> Int = { apply(1) { it + 1 } }\n",
        "NestedInlineReturnOwnership",
    );
    let outer_inline = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                inline_body: Some(inline),
                ..
            } => Some(*inline),
            _ => None,
        })
        .next_back()
        .expect("outer lambda inline template");

    assert!(!expression_subtree_contains(
        &ir,
        outer_inline,
        &|expression, node| {
            !matches!(node, IrExpr::Return(_)) && ir.checked_return_depths.contains_key(&expression)
        }
    ));
}

#[test]
fn lambda_with_non_local_return_has_no_standalone_callable_realization() {
    let ir = lower_single_source(
        "inline fun outer(block: () -> Unit) { inner { block() } }\n\
         inline fun inner(block: () -> Unit) { block() }\n\
         fun probe(hit: Boolean): Int {\n\
             outer { if (hit) return 7 }\n\
             return -1\n\
         }\n",
        "NonLocalReturnRealization",
    );
    let implementations = ir
        .lambda_origins
        .iter()
        .filter_map(|(function, origin)| {
            (origin.implementation_name == "probe").then_some(*function)
        })
        .collect::<Vec<_>>();
    assert_eq!(implementations.len(), 1, "probe lambda implementations");
    assert!(
        ir.inline_only_fns.contains(&implementations[0]),
        "a non-local return cannot be emitted in the lambda's own callable descriptor"
    );
    let probe = ir
        .functions
        .iter()
        .find(|function| function.name == "probe")
        .and_then(|function| function.body)
        .expect("probe body");
    assert!(!expression_subtree_contains(
        &ir,
        probe,
        &|_, expression| matches!(
            expression,
            IrExpr::Lambda { impl_fn, .. } if *impl_fn == implementations[0]
        )
    ));
}

#[test]
fn unit_lambda_callable_materializes_unit_on_an_explicit_bare_return() {
    let ir = lower_single_source(
        "fun make(): () -> Unit = fun() { return }\n",
        "UnitLambdaBareReturn",
    );
    let implementation = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
            _ => None,
        })
        .expect("anonymous Unit function value");
    let callable = ir.functions[implementation as usize]
        .body
        .expect("materialized callable body");

    assert_eq!(
        ir.functions[implementation as usize].ret,
        Ty::obj("kotlin/Unit")
    );
    assert!(expression_subtree_contains(
        &ir,
        callable,
        &|_, expression| {
            let IrExpr::Return(Some(value)) = expression else {
                return false;
            };
            matches!(ir.expr(*value), IrExpr::UnitInstance)
        }
    ));
}
