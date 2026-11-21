use krusty::ir::{IrEnclosure, IrExpr, IrFile, IrFunction, IrGenericSig, IrTypeParameter};
use krusty::types::{Ty, TypeVariance};

use super::super::lower_alone;
use super::Unsupported;

/// The reified parameter both the template and the copy test, by one semantic identity.
fn reified_parameter() -> IrTypeParameter {
    IrTypeParameter {
        name: "T".to_owned(),
        semantic_name: "\0tp:reified-template:T".to_owned(),
        bounds: Vec::new(),
        variance: TypeVariance::Invariant,
        reified: true,
        captured_from_enclosing: false,
    }
}

/// A one-parameter lambda implementation whose body is `return it is T`.
fn type_test_implementation(ir: &mut IrFile, name: &str) -> u32 {
    let tested = reified_parameter().ty();
    let operand = ir.add_expr(IrExpr::GetValue(0));
    ir.set_logical_type(operand, Ty::nullable(Ty::obj("kotlin/Any")));
    let test = ir.add_expr(IrExpr::TypeOp {
        op: krusty::ir::IrTypeOp::InstanceOf,
        arg: operand,
        type_operand: tested,
    });
    ir.set_logical_type(test, Ty::Boolean);
    let returned = ir.add_expr(IrExpr::Return(Some(test)));
    ir.set_logical_type(returned, Ty::Nothing);
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    ir.add_fun(IrFunction {
        name: name.to_owned(),
        params: vec![Ty::nullable(Ty::obj("kotlin/Any"))],
        ret: Ty::Boolean,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    })
}

/// `inline fun <reified T> checks()`, which declares `{ it: Any? -> it is T }`: the declaration,
/// its own lambda implementation, and, with `copy`, one expansion's specialized implementation that still tests
/// the same `T`.
fn reified_template_file(copy: bool) -> IrFile {
    let mut ir = IrFile::default();
    let result = ir.add_expr(IrExpr::Const(krusty::ir::IrConst::Boolean(true)));
    ir.set_logical_type(result, Ty::Boolean);
    let returned = ir.add_expr(IrExpr::Return(Some(result)));
    ir.set_logical_type(returned, Ty::Nothing);
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    let declaration = ir.add_fun(IrFunction {
        name: "checks".to_owned(),
        params: Vec::new(),
        ret: Ty::Boolean,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.signatures.insert(
        declaration,
        IrGenericSig {
            type_params: vec![reified_parameter()],
            params: Vec::new(),
            ret: Some(Ty::Boolean),
            supers: Vec::new(),
        },
    );
    let template = type_test_implementation(&mut ir, "checks$lambda");
    let node = ir.add_expr(IrExpr::Lambda {
        impl_fn: template,
        arity: 1,
        captures: Vec::new(),
        sam: None,
        inline_body: None,
    });
    ir.callable_reference_enclosures
        .insert(node, IrEnclosure::Function(declaration));
    if copy {
        let specialized = type_test_implementation(&mut ir, "caller$lambda");
        ir.specialized_functions.insert(
            specialized,
            krusty::ir::IrSpecializedFunction {
                source: template,
                caller_declaration: krusty::fir::DeclarationId::from_raw(0),
                caller: None,
                caller_is_default: false,
                caller_source_name: "caller".to_owned(),
                inline_callee: krusty::ir::IrInlineCallee::Module(
                    krusty::fir::CallableId::from_raw(0),
                ),
                inline_callee_source_name: "checks".to_owned(),
                parent: None,
                consumed_inline_capture: false,
            },
        );
    }
    ir
}

fn lower(mut ir: IrFile) -> Result<(), Unsupported> {
    let properties = krusty::backend::local_properties::realize(&mut ir).unwrap();
    let mut started = super::super::super::Started::new(crate::target::WasmTarget::Js);
    lower_alone(&ir, &properties, &mut started).map(|_| ())
}

/// The declaration's own lambda traps at its test of `T`; a specialized copy that still tests
/// the same `T` is executable code and keeps the refusal.
#[test]
fn only_the_declaration_template_traps_at_its_reified_type_test() {
    assert_eq!(lower(reified_template_file(false)), Ok(()));
    assert_eq!(
        lower(reified_template_file(true)),
        Err(format!(
            "a type test against `{:?}`",
            reified_parameter().ty()
        ))
    );
}

/// How the anonymous function's implementation is created in [`captured_signature_file`].
#[derive(Clone, Copy)]
enum Creation {
    /// By its one node in the declaration's body.
    Declared,
    /// By a node an expansion copied into its caller.
    CopiedNode,
    /// By a second node beside the declaration's own.
    SecondNode,
}

/// `inline fun <reified T> checks()`, which declares `fun(it: Any?) = it is T`: the anonymous
/// function's signature captures the outer `T` with its identity and reified capability, as
/// lifting a local or anonymous function signs it.
fn captured_signature_file(creation: Creation) -> IrFile {
    let mut ir = reified_template_file(false);
    let declaration = 0;
    let template = 1;
    ir.signatures.insert(
        template,
        IrGenericSig {
            type_params: vec![IrTypeParameter {
                captured_from_enclosing: true,
                ..reified_parameter()
            }],
            params: vec![Ty::nullable(Ty::obj("kotlin/Any"))],
            ret: Some(Ty::Boolean),
            supers: Vec::new(),
        },
    );
    let node = ir
        .exprs
        .iter()
        .position(|expression| matches!(expression, IrExpr::Lambda { impl_fn, .. } if *impl_fn == template))
        .expect("the declaration creates its anonymous function") as u32;
    match creation {
        Creation::Declared => {}
        Creation::CopiedNode => ir.mark_inline_copy(node),
        Creation::SecondNode => {
            let second = ir.add_expr(IrExpr::Lambda {
                impl_fn: template,
                arity: 1,
                captures: Vec::new(),
                sam: None,
                inline_body: None,
            });
            ir.callable_reference_enclosures
                .insert(second, IrEnclosure::Function(declaration));
        }
    }
    ir
}

/// A captured reified parameter in the implementation's own signature does not make it the
/// declaration: only its one uncopied node leads the walk to the declaration that owns `T`.
#[test]
fn a_captured_reified_signature_traps_only_through_its_declaring_node() {
    assert_eq!(lower(captured_signature_file(Creation::Declared)), Ok(()));
    let refused = Err(format!(
        "a type test against `{:?}`",
        reified_parameter().ty()
    ));
    assert_eq!(
        lower(captured_signature_file(Creation::CopiedNode)),
        refused
    );
    assert_eq!(
        lower(captured_signature_file(Creation::SecondNode)),
        refused
    );
}
