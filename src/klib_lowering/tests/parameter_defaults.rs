//! Serialized parameter defaults, published as checked FIR lowering publishes a source
//! declaration's.

use super::super::DependencyBodyUnit;
use super::demo_package::{demo, source_body_with_stdlib, source_defaults_with_stdlib, T};
use super::{lowered_body, public_symbol, render, validated};
use crate::ir::{Callee, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrArguments, KlibIrExprKind, KlibIrMemberAccess};
use crate::metadata::klib_ir::KlibIrSymbolKind;

/// kotlinc's `fun f(a: Int, b: Int = a, c: Int = 7): Int = b`: each default is lowered in the
/// declaration's own frame, where it may read an earlier parameter, and only a parameter that
/// declares one has a slot filled, exactly as checked FIR lowering lowers the source.
#[test]
fn a_body_publishes_each_serialized_default_in_its_parameter_slot() {
    let fixture = demo(&[("f", &[T::Int, T::Int, T::Int], T::Int)], |_, body| {
        let earlier = body.param(0);
        body.default(1, earlier);
        let seven = body.int(7);
        body.default(2, seven);
        let result = body.param(1);
        Some(vec![body.ret(result)])
    });
    let mut unit = DependencyBodyUnit::default();
    let function = fixture
        .lower(&mut unit, "f", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = "fun f(a: Int, b: Int = a, c: Int = 7): Int {\n return b\n}\n";
    assert_eq!(
        lowered_body(&unit, function),
        source_body_with_stdlib(source, "f", 3)
    );
    let defaults = unit
        .ir()
        .param_defaults(function)
        .expect("the declaration publishes its defaults")
        .iter()
        .map(|default| default.map(|default| render(unit.ir(), default)))
        .collect::<Vec<_>>();
    assert_eq!(defaults, source_defaults_with_stdlib(source, "f", 3));
    assert_eq!(defaults.iter().flatten().count(), 2);
}

/// A declaration without a serialized default publishes no defaults at all, as a source one does.
#[test]
fn a_body_without_defaults_publishes_none() {
    let fixture = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let result = body.param(0);
        Some(vec![body.ret(result)])
    });
    let mut unit = DependencyBodyUnit::default();
    let function = fixture
        .lower(&mut unit, "f", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(unit.ir().param_defaults(function), None);
}

/// kotlinc's `fun g(a: Int, b: Int = a): Int = b` and `fun f(x: Int): Int = g(x)`: the serialized
/// call leaves `b` out, and lowers as a source call of a function of its own file leaving it to
/// its default does, carrying the supplied argument and the omitted parameter.
fn omitting_fixture(g_declares_the_default: bool) -> super::demo_package::Demo {
    demo(
        &[("g", &[T::Int, T::Int], T::Int), ("f", &[T::Int], T::Int)],
        move |name, body| match name {
            "g" => {
                if g_declares_the_default {
                    let earlier = body.param(0);
                    body.default(1, earlier);
                }
                let result = body.param(1);
                Some(vec![body.ret(result)])
            }
            "f" => {
                let x = body.param(0);
                let signature = body.signatures["g"].clone();
                let call = body.expr(
                    T::Int,
                    KlibIrExprKind::Call {
                        access: KlibIrMemberAccess {
                            symbol: public_symbol(KlibIrSymbolKind::Function, signature),
                            arguments: KlibIrArguments::Flat(vec![Some(x), None]),
                            type_arguments: Vec::new(),
                            origin: None,
                        },
                        super_qualifier: None,
                    },
                );
                Some(vec![body.ret(call)])
            }
            _ => None,
        },
    )
}

#[test]
fn a_call_leaving_a_parameter_to_its_default_carries_the_omission() {
    let fixture = omitting_fixture(true);
    let mut unit = DependencyBodyUnit::default();
    let function = fixture
        .lower(&mut unit, "f", &["g"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = "fun g(a: Int, b: Int = a): Int {\n return b\n}\n\
                  fun f(x: Int): Int {\n return g(x)\n}\n";
    assert_eq!(
        lowered_body(&unit, function),
        source_body_with_stdlib(source, "f", 1)
    );
    let ir = unit.ir();
    let calls = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call { callee, args, .. } => match &**callee {
                Callee::LocalWithDefaults { function, defaults } => {
                    Some((*function, defaults.to_vec(), args.len()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(g, omitted, supplied)] = calls.as_slice() else {
        panic!("one call leaves an argument out: {calls:?}");
    };
    assert_eq!((omitted.as_slice(), *supplied), ([1].as_slice(), 1));
    assert_eq!(
        ir.param_defaults(*g)
            .expect("g publishes its default")
            .iter()
            .map(Option::is_some)
            .collect::<Vec<_>>(),
        [false, true]
    );
}

/// An argument left out of a call of a parameter without a default has nothing to fill it: the
/// body declines rather than inventing a value.
#[test]
fn a_call_leaving_out_an_argument_without_a_default_declines() {
    let fixture = omitting_fixture(false);
    let mut unit = DependencyBodyUnit::default();
    let decline = fixture
        .lower(&mut unit, "f", &["g"])
        .expect_err("g declares no default for `b`");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.f` (it uses a call that leaves a parameter to its default)"
    );
    assert!(unit.ir().functions.is_empty());
}

/// kotlinc's `fun a(a: Int): Int = b(a)` beside `fun b(a: Int, b: Int = a): Int = b`.
const DEFAULTED_AND_CALLER_SOURCE: &str = "fun b(a: Int, b: Int = a): Int {\n return b\n}\n\
     fun a(a: Int): Int {\n return b(a)\n}\n";

/// [`DEFAULTED_AND_CALLER_SOURCE`] as serialized trees: `b`'s second parameter defaults to its
/// first, and `a` calls `b` leaving it out.
fn defaulted_and_caller() -> super::demo_package::Demo {
    demo(
        &[("b", &[T::Int, T::Int], T::Int), ("a", &[T::Int], T::Int)],
        |name, body| {
            let value = if name == "a" {
                let argument = body.param(0);
                body.call_omitting("b", vec![Some(argument), None], T::Int)
            } else {
                let earlier = body.param(0);
                body.default(1, earlier);
                body.param(1)
            };
            Some(vec![body.ret(value)])
        },
    )
}

/// A body call that leaves `b`'s second parameter to its default calls `b` with that omission,
/// as checked FIR lowering lowers the source call, and the shared default-argument lowering
/// reaches `b`'s entry from it.
#[test]
fn a_body_call_that_leaves_a_parameter_to_its_default_carries_the_omission() {
    let fixture = defaulted_and_caller();
    let mut unit = DependencyBodyUnit::default();
    let caller = fixture
        .lower(&mut unit, "a", &["a", "b"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, caller),
        source_body_with_stdlib(DEFAULTED_AND_CALLER_SOURCE, "a", 1)
    );
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call { callee, .. } => match &**callee {
                Callee::LocalWithDefaults { function, defaults } => Some((
                    unit.ir().fn_source_names[function].clone(),
                    defaults.to_vec(),
                )),
                other => panic!("the body makes no other call: {other:?}"),
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, [("b".to_owned(), vec![1])]);
}

/// The callee of that graph lowers its body and publishes its whole default vector exactly as
/// checked FIR lowering does for the same source.
#[test]
fn the_defaulted_callee_publishes_its_defaults_as_checked_fir_lowering_does() {
    let fixture = defaulted_and_caller();
    let mut unit = DependencyBodyUnit::default();
    let callee = fixture
        .lower(&mut unit, "b", &["b"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, callee),
        source_body_with_stdlib(DEFAULTED_AND_CALLER_SOURCE, "b", 2)
    );
    let defaults = unit
        .ir()
        .param_defaults(callee)
        .expect("the declaration publishes its defaults")
        .iter()
        .map(|default| default.map(|default| render(unit.ir(), default)))
        .collect::<Vec<_>>();
    let expected = source_defaults_with_stdlib(DEFAULTED_AND_CALLER_SOURCE, "b", 2);
    assert!(matches!(expected.as_slice(), [None, Some(_)]));
    assert_eq!(defaults, expected);
}

/// The caller of that graph declares no default and publishes none, as its source does.
#[test]
fn the_caller_without_defaults_publishes_none() {
    let fixture = defaulted_and_caller();
    let mut unit = DependencyBodyUnit::default();
    let caller = fixture
        .lower(&mut unit, "a", &["a", "b"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(unit.ir().param_defaults(caller), None);
}

/// A class model in which `String` is a `CharSequence`, as the calling compilation's
/// declarations state.
struct StringIsCharSequence;

impl crate::assignable::TypeOracle for StringIsCharSequence {
    fn direct_supertypes(&self, ty: crate::types::Ty) -> Vec<crate::types::Ty> {
        if ty == T::String.semantic() {
            vec![T::CharSequence.semantic(), T::Any.semantic()]
        } else if ty == T::CharSequence.semantic() {
            vec![T::Any.semantic()]
        } else {
            Vec::new()
        }
    }
}

/// kotlinc's `fun f(separator: CharSequence = ", "): CharSequence = separator`, the shape of the
/// stdlib's `joinToString` defaults: the `String` default is a value of a subtype of its
/// parameter, which the subtyping facts prove, and is published converted to the parameter's type
/// as the source default is. Without those facts the same default is a conversion the body does
/// not state.
#[test]
fn a_default_of_a_subtype_of_its_parameter_is_converted_at_the_parameter() {
    let fixture = demo(&[("f", &[T::CharSequence], T::CharSequence)], |_, body| {
        let separator = body.string(", ");
        body.default(0, separator);
        let result = body.param(0);
        Some(vec![body.ret(result)])
    });
    let oracle = StringIsCharSequence;
    let facts = fixture.callees(&[]).with_type_oracle(&oracle);
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(fixture.callable("f"), &fixture.bodies, &facts)
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = "fun f(separator: CharSequence = \", \"): CharSequence {\n return separator\n}\n";
    assert_eq!(
        lowered_body(&unit, function),
        source_body_with_stdlib(source, "f", 1)
    );
    let defaults = unit
        .ir()
        .param_defaults(function)
        .expect("the declaration publishes its default")
        .iter()
        .map(|default| default.map(|default| render(unit.ir(), default)))
        .collect::<Vec<_>>();
    assert_eq!(defaults, source_defaults_with_stdlib(source, "f", 1));
    assert_eq!(
        fixture.declined("f", &[]),
        "the KLIB body of `demo.f` (it converts a value implicitly)"
    );
}
