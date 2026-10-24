//! Serialized lexical captures and invocation parameters keep distinct value frames.

use super::demo_package::{demo, Body, T};
use super::{base, local_symbol, public_symbol, validated};
use crate::ir::{IrExpr, IrParameterRole};
use crate::klib_lowering::DependencyBodyUnit;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrBody, KlibIrExprId, KlibIrExprKind, KlibIrFunction, KlibIrParameter, KlibIrStatement,
    KlibIrType, KlibIrTypeArgument, KlibIrVariance,
};
use crate::metadata::klib_ir::KlibIrSymbolKind;
use crate::types::Ty;

fn closure(
    body: &mut Body,
    rich: Option<usize>,
    first_read: Option<KlibIrExprId>,
    foreign: bool,
    form: Option<&str>,
) -> KlibIrExprId {
    let symbol = local_symbol(KlibIrSymbolKind::Function, 90000);
    let int = body.ty(T::Int);
    let params = (0..2)
        .map(|ordinal| KlibIrParameter {
            base: base(local_symbol(
                KlibIrSymbolKind::ValueParameter,
                90001 + ordinal,
            )),
            // Deliberately shadows the enclosing parameter's spelling.
            name: "a".to_owned(),
            ty: int,
            vararg_element_type: None,
            default_value: None,
        })
        .collect::<Vec<_>>();
    let captured_int = first_read.unwrap_or_else(|| body.param(0));
    let captured_string = body.param(1);
    let own_first = body.get(&params[0].base.symbol, T::Int);
    let own_second = body.get(
        &if foreign {
            local_symbol(KlibIrSymbolKind::Variable, 99999)
        } else {
            params[1].base.symbol.clone()
        },
        T::Int,
    );
    let returned = body.expr(
        T::Nothing,
        KlibIrExprKind::Return {
            target: symbol.clone(),
            value: own_second,
        },
    );
    let function = body.arena.push_function(KlibIrFunction {
        base: base(symbol),
        name: "anonymous".to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: params,
        return_type: int,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(vec![
            KlibIrStatement::Expression(captured_int),
            KlibIrStatement::Expression(captured_string),
            KlibIrStatement::Expression(own_first),
            KlibIrStatement::Expression(returned),
        ])),
    });
    let mut function_type = T::Any.klib();
    let KlibIrType::Simple {
        classifier,
        arguments,
        ..
    } = &mut function_type
    else {
        unreachable!()
    };
    *classifier = public_symbol(
        KlibIrSymbolKind::Class,
        KlibPublicIdSignature::classifier(crate::types::type_name("kotlin/Function2")),
    );
    *arguments = vec![
        KlibIrTypeArgument::Type {
            variance: KlibIrVariance::Invariant,
            ty: int
        };
        3
    ];
    let ty = body.arena.push_type(function_type);
    let kind = if let Some(arity) = rich {
        let owner = format!("Function{arity}");
        let overridden = super::demo_package::operator(
            &["kotlin"],
            Some(&owner),
            "invoke",
            &vec![T::NullableAny.kotlin(); arity],
        );
        KlibIrExprKind::RichFunctionReference {
            bound_values: Vec::new(),
            reflection_target: None,
            overridden: public_symbol(KlibIrSymbolKind::Function, overridden.0),
            invoke: function,
            flags: 0,
            origin: form.map(str::to_owned),
        }
    } else {
        KlibIrExprKind::FunctionExpression {
            function,
            origin: form.map(str::to_owned),
        }
    };
    body.arena.push_expr(Some(ty), kind)
}

#[test]
fn serialized_closure_frames_separate_captures_from_own_parameters() {
    use crate::ir::IrCapturingCallable::{AnonymousFunction, Lambda};
    for (rich, form, capturer) in [
        (None, "LAMBDA", Lambda),
        (Some(2), "LAMBDA", Lambda),
        (None, "ANONYMOUS_FUNCTION", AnonymousFunction),
        (Some(2), "ANONYMOUS_FUNCTION", AnonymousFunction),
    ] {
        let fixture = demo(&[("a", &[T::Int, T::String], T::Int)], |_, body| {
            let lambda = closure(body, rich, None, false, Some(form));
            let answer = body.int(0);
            Some(vec![KlibIrStatement::Expression(lambda), body.ret(answer)])
        });
        let mut unit = DependencyBodyUnit::default();
        fixture
            .lower(&mut unit, "a", &[])
            .expect("immutable closure lowers");
        validated(unit.ir());
        let ir = unit.ir();
        let lambdas = ir
            .exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::Lambda {
                    impl_fn,
                    arity,
                    captures,
                    ..
                } => Some((*impl_fn, *arity, captures)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(implementation, arity, captures)] = lambdas.as_slice() else {
            panic!("expected exactly one closure")
        };
        assert_eq!(*arity, 2);
        assert_eq!(ir.lambda_own_params_from[implementation], 2);
        assert_eq!(
            ir.method_visibility(*implementation),
            crate::types::Visibility::Private
        );
        assert_eq!(captures.len(), 2);
        assert_eq!(
            captures
                .iter()
                .map(|id| match ir.exprs[*id as usize] {
                    IrExpr::GetValue(slot) => slot,
                    _ => panic!("a capture reads its exact enclosing binding"),
                })
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            ir.functions[*implementation as usize].params,
            vec![Ty::Int, Ty::String, Ty::Int, Ty::Int]
        );
        let identities = &ir.fn_params[implementation].identities;
        for identity in &identities[..2] {
            let IrParameterRole::CapturedValue { capture, .. } = identity.role else {
                panic!("these captures are values")
            };
            assert_eq!(capture.capturer, capturer);
        }
        assert!(matches!(
            identities[0].role,
            IrParameterRole::CapturedValue { ordinal: 0, .. }
        ));
        assert!(matches!(
            identities[1].role,
            IrParameterRole::CapturedValue { ordinal: 1, .. }
        ));
        assert_eq!(identities[2].role, IrParameterRole::Value);
        assert_eq!(identities[3].role, IrParameterRole::Value);
        assert_eq!(
            crate::ir::read_values(ir, ir.functions[*implementation as usize].body.unwrap()),
            [0, 1, 2, 3].into_iter().collect()
        );
        assert_eq!(
            identities
                .iter()
                .map(|identity| identity.source_name.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("a"), Some("b"), Some("a"), Some("a")]
        );
    }
}

#[test]
fn mutable_serialized_capture_declines_without_leaking_a_frame() {
    let fixture = demo(&[("a", &[T::Int, T::String], T::Int)], |_, body| {
        let initial = body.int(7);
        let (declaration, symbol) = body.var(T::Int, initial);
        let read = body.get(&symbol, T::Int);
        let lambda = closure(body, Some(2), Some(read), false, Some("LAMBDA"));
        let answer = body.int(0);
        Some(vec![
            declaration,
            KlibIrStatement::Expression(lambda),
            body.ret(answer),
        ])
    });
    let mut unit = DependencyBodyUnit::default();
    let decline = fixture
        .lower(&mut unit, "a", &[])
        .expect_err("mutable cell is separate work");
    assert_eq!(
        decline.reason(),
        &crate::klib_lowering::KlibBodyDeclineReason::UnsupportedOperation(
            "a mutable closure capture"
        )
    );
    super::demo_package::assert_unit_is_empty(&unit);
    assert!(unit.ir().lambda_own_params_from.is_empty());
    assert!(unit.ir().method_visibilities.is_empty());
}

#[test]
fn a_foreign_read_rolls_back_an_already_declared_serialized_closure() {
    let fixture = demo(&[("a", &[T::Int, T::String], T::Int)], |_, body| {
        let lambda = closure(body, Some(2), None, true, Some("LAMBDA"));
        let answer = body.int(0);
        Some(vec![KlibIrStatement::Expression(lambda), body.ret(answer)])
    });
    let mut unit = DependencyBodyUnit::default();
    let decline = fixture
        .lower(&mut unit, "a", &[])
        .expect_err("foreign symbol cannot capture by spelling");
    assert_eq!(
        decline.reason(),
        &crate::klib_lowering::KlibBodyDeclineReason::ForeignValue
    );
    super::demo_package::assert_unit_is_empty(&unit);
    assert!(unit.ir().lambda_own_params_from.is_empty());
    assert!(unit.ir().method_visibilities.is_empty());
}

#[test]
fn a_rich_closure_authenticates_the_invocation_role_of_its_override() {
    let fixture = demo(&[("a", &[T::Int, T::String], T::Int)], |_, body| {
        let lambda = closure(body, Some(1), None, false, Some("LAMBDA"));
        let answer = body.int(0);
        Some(vec![KlibIrStatement::Expression(lambda), body.ret(answer)])
    });
    let mut unit = DependencyBodyUnit::default();
    let decline = fixture
        .lower(&mut unit, "a", &[])
        .expect_err("Function1 is not Function2.invoke");
    assert_eq!(
        decline.reason(),
        &crate::klib_lowering::KlibBodyDeclineReason::SignatureMismatch(
            "a rich closure does not override its function invocation".to_owned()
        )
    );
    super::demo_package::assert_unit_is_empty(&unit);
}

#[test]
fn an_unrecorded_closure_source_form_is_not_inferred_from_its_encoding() {
    let fixture = demo(&[("a", &[T::Int, T::String], T::Int)], |_, body| {
        let lambda = closure(body, Some(2), None, false, None);
        let answer = body.int(0);
        Some(vec![KlibIrStatement::Expression(lambda), body.ret(answer)])
    });
    let mut unit = DependencyBodyUnit::default();
    let decline = fixture
        .lower(&mut unit, "a", &[])
        .expect_err("source form was not serialized");
    assert_eq!(
        decline.reason(),
        &crate::klib_lowering::KlibBodyDeclineReason::UnsupportedOperation(
            "a closure without a serialized source form"
        )
    );
    super::demo_package::assert_unit_is_empty(&unit);
}
