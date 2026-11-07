use super::*;
use crate::ir::IrTypeOp;

#[test]
fn converting_a_checked_exhaustive_when_without_else_preserves_its_proof() {
    let mut ir = crate::fir_lower::tests::lower_single_source(
        "fun select(flag: Boolean): Int = when (flag) { true -> 1; false -> 2 }\n",
        "ExhaustiveWhenConversion",
    );
    let original = ir
        .exprs
        .iter()
        .enumerate()
        .find_map(|(expression, node)| {
            matches!(node, IrExpr::When { .. }).then_some(expression as ExprId)
        })
        .expect("the complete checked source lowers its value when");
    let IrExpr::When { branches } = ir.expr(original) else {
        unreachable!();
    };
    let original_branches = branches.clone();
    assert_eq!(original_branches.len(), 2);
    assert!(original_branches
        .iter()
        .all(|(condition, _)| condition.is_some()));
    assert_eq!(ir.whens.exhaustive.get(&original), Some(&Ty::Int));
    let result = Ty::nullable(Ty::Int);
    let conversion = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: original,
        type_operand: result,
    });
    let converted = normalize_value_when(&mut ir, conversion)
        .expect("the actual coroutine normalizer distributes the conversion");
    assert_ne!(converted, original);
    assert_eq!(ir.when_result_type(converted), Some(result));
    assert_eq!(ir.whens.exhaustive.get(&converted), Some(&result));
    assert_eq!(ir.whens.exhaustive.get(&original), Some(&Ty::Int));
    let IrExpr::When { branches } = ir.expr(converted) else {
        panic!("the converted expression remains a when");
    };
    assert_eq!(branches.len(), original_branches.len());
    for ((condition, body), (original_condition, original_body)) in
        branches.iter().zip(&original_branches)
    {
        assert_eq!(condition, original_condition);
        assert!(condition.is_some(), "normalization must not invent an else");
        let (original_stmts, original_value) = match ir.expr(*original_body) {
            IrExpr::Block {
                stmts,
                value: Some(value),
            } => (stmts.clone(), *value),
            _ => (Vec::new(), *original_body),
        };
        let (stmts, value) = match ir.expr(*body) {
            IrExpr::Block {
                stmts,
                value: Some(value),
            } => (stmts.clone(), *value),
            _ => (Vec::new(), *body),
        };
        assert_eq!(stmts, original_stmts);
        assert!(matches!(
            ir.expr(value),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } if *arg == original_value && *type_operand == result
        ));
    }
}
