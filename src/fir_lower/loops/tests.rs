use crate::ir::{Callee, IrExpr, IrIntrinsic};
use crate::types::Ty;

use super::super::tests::lower_single_source;

#[test]
fn covariant_array_loop_publishes_the_element_read_type() {
    let ir = lower_single_source(
        "class Element\n\
         fun scan(values: Array<out Element>) { for (element in values) { element } }\n",
        "CovariantArrayLoop",
    );
    let declaration = ir
        .value_names
        .iter()
        .find_map(|(&expression, name)| (name == "element").then_some(expression))
        .expect("named loop variable declaration");
    assert!(matches!(
        ir.expr(declaration),
        IrExpr::Variable {
            ty: Ty::Obj(name, _),
            init: Some(element),
            ..
        } if name.matches("Element")
            && matches!(
                ir.expr(*element),
                IrExpr::Call {
                    callee: Callee::Intrinsic {
                        operation: IrIntrinsic::ArrayGet,
                        ret: Ty::Obj(element_name, _),
                    },
                    ..
                } if element_name.matches("Element")
            )
    ));
}

#[test]
fn star_projected_array_loop_publishes_the_element_upper_bound() {
    let ir = lower_single_source(
        "fun scan(values: Array<*>) { for (element in values) { element } }\n",
        "StarProjectedArrayLoop",
    );
    let declaration = ir
        .value_names
        .iter()
        .find_map(|(&expression, name)| (name == "element").then_some(expression))
        .expect("named loop variable declaration");
    let expected = Ty::nullable(Ty::obj("kotlin/Any"));
    assert!(matches!(
        ir.expr(declaration),
        IrExpr::Variable {
            ty,
            init: Some(element),
            ..
        } if *ty == expected
            && matches!(
                ir.expr(*element),
                IrExpr::Call {
                    callee: Callee::Intrinsic {
                        operation: IrIntrinsic::ArrayGet,
                        ret,
                    },
                    ..
                } if *ret == expected
            )
    ));
}
