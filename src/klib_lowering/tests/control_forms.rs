//! Lowering of the KLIB control and value forms: `&&` and `||`, `if` and `when` without `else`,
//! nested blocks, bodies without a final `return`, `throw`, string templates, loops with their
//! jumps, and type operators. Each lowered body is compared with checked FIR lowering of the
//! equivalent source, on hand-built trees and on the Kotlin/Native stdlib bodies that use the
//! form.

use super::super::*;
use super::demo_package::{demo, lowered_f, relation, source_body, Body, Demo, Shape, T};
use super::{distribution_root, frozen_function, lowered_body, stdlib, validated};
use crate::metadata::klib_ir::tree::{
    KlibIrArguments, KlibIrExprId, KlibIrExprKind, KlibIrMemberAccess, KlibIrStatement,
    KlibIrTypeOperator,
};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSignature, KlibIrSymbolKind};

/// The rendered body of `name` in a unit that lowered it with the frozen `callees`.
fn lowered(demo: &Demo, name: &str, callees: &[&str]) -> String {
    let mut unit = DependencyBodyUnit::default();
    let function = demo
        .lower(&mut unit, name, callees)
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    lowered_body(&unit, function)
}

/// `f(a: Boolean, b: Boolean): Boolean` returning the `when` `build` makes of its parameters.
fn boolean_operator(build: fn(&mut Body, KlibIrExprId, KlibIrExprId) -> KlibIrExprId) -> Demo {
    demo(
        &[("f", &[T::Boolean, T::Boolean], T::Boolean)],
        move |_, body| {
            let a = body.param(0);
            let b = body.param(1);
            let value = build(body, a, b);
            Some(vec![body.ret(value)])
        },
    )
}

// --- `&&` and `||` -----------------------------------------------------------------------------

#[test]
fn short_circuit_operators_lower_as_the_source_operators_do() {
    let and = boolean_operator(|body, a, b| {
        let otherwise = body.otherwise();
        let constant = body.boolean(false);
        body.branches("ANDAND", vec![(a, b), (otherwise, constant)], T::Boolean)
    });
    let lowered = lowered_f(&and);
    assert_eq!(
        lowered,
        source_body(
            "fun f(a: Boolean, b: Boolean): Boolean {\n return a && b\n}\n",
            "f",
            2
        )
    );
    assert_eq!(
        lowered,
        "scope {scope {return@Some(0) and when[v0!: Boolean -> v1!: Boolean, \
         else -> Boolean(false)]: Boolean: Nothing}: Nothing}"
    );
    let or = boolean_operator(|body, a, b| {
        let constant = body.boolean(true);
        let otherwise = body.otherwise();
        body.branches("OROR", vec![(a, constant), (otherwise, b)], T::Boolean)
    });
    assert_eq!(
        lowered_f(&or),
        source_body(
            "fun f(a: Boolean, b: Boolean): Boolean {\n return a || b\n}\n",
            "f",
            2
        )
    );
}

#[test]
fn a_short_circuit_with_another_constant_declines() {
    let malformed = boolean_operator(|body, a, b| {
        let otherwise = body.otherwise();
        let constant = body.boolean(true);
        body.branches("ANDAND", vec![(a, b), (otherwise, constant)], T::Boolean)
    });
    assert_eq!(
        malformed.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         a short-circuit operator's constant branch is not its operator's)"
    );
}

#[test]
fn a_short_circuit_condition_of_an_if_lowers_as_the_source_does() {
    let demo = demo(&[("f", &[T::Boolean, T::Boolean], T::Int)], |_, body| {
        let a = body.param(0);
        let b = body.param(1);
        let otherwise = body.otherwise();
        let constant = body.boolean(false);
        let both = body.branches("ANDAND", vec![(a, b), (otherwise, constant)], T::Boolean);
        let one = body.int(1);
        let returned = body.return_(one);
        let guard = body.branches("IF", vec![(both, returned)], T::Unit);
        let zero = body.int(0);
        Some(vec![KlibIrStatement::Expression(guard), body.ret(zero)])
    });
    let source = "fun f(a: Boolean, b: Boolean): Int {\n if (a && b) return 1\n return 0\n}\n";
    assert_eq!(lowered_f(&demo), source_body(source, "f", 2));
}

// --- `if` and `when` without `else` ------------------------------------------------------------

/// `g(a: Int): Int` returning its argument, `h(a: Int)` doing nothing, and `f(a: Boolean, b:
/// Int)`, whose body `build` makes.
fn with_callees(build: fn(&mut Body) -> Vec<KlibIrStatement>) -> Demo {
    let shapes: [Shape; 3] = [
        ("g", &[T::Int], T::Int),
        ("h", &[T::Int], T::Unit),
        ("f", &[T::Boolean, T::Int], T::Unit),
    ];
    demo(&shapes, move |name, body| match name {
        "g" => {
            let a = body.param(0);
            Some(vec![body.ret(a)])
        }
        "h" => Some(Vec::new()),
        _ => Some(build(body)),
    })
}

const CALLEES_SOURCE: &str = "fun g(a: Int): Int {\n return a\n}\nfun h(a: Int) {\n}\n";

fn lowered_with_callees(demo: &Demo) -> String {
    lowered(demo, "f", &["g", "h"])
}

#[test]
fn an_if_without_else_lowers_as_the_source_statement_does() {
    // `if (a) g(b)`: the KLIB coerces the branch's `Int` to `Unit`.
    let value = with_callees(|body| {
        let a = body.param(0);
        let b = body.param(1);
        let call = body.call("g", vec![b], T::Int);
        let coerced = body.coerced(call);
        vec![KlibIrStatement::Expression(body.branches(
            "IF",
            vec![(a, coerced)],
            T::Unit,
        ))]
    });
    let source = format!("{CALLEES_SOURCE}fun f(a: Boolean, b: Int) {{\n if (a) g(b)\n}}\n");
    let lowered = lowered_with_callees(&value);
    assert_eq!(lowered, source_body(&source, "f", 2));
    assert_eq!(
        lowered,
        "scope {scope { => exhaustive:Unit when[v0!: Boolean -> {call g(v1!: Int): Int => Unit}: \
         Unit, else -> {}: Unit]: Unit}: Unit}"
    );
    // `if (a) h(b); h(b)`: a `Unit` call is given the `Unit` value too, and the `if` is a
    // statement before the block's value.
    let effect = with_callees(|body| {
        let a = body.param(0);
        let b = body.param(1);
        let call = body.call("h", vec![b], T::Unit);
        let conditional = body.branches("IF", vec![(a, call)], T::Unit);
        let b = body.param(1);
        let last = body.call("h", vec![b], T::Unit);
        vec![
            KlibIrStatement::Expression(conditional),
            KlibIrStatement::Expression(last),
        ]
    });
    let source = format!("{CALLEES_SOURCE}fun f(a: Boolean, b: Int) {{\n if (a) h(b)\n h(b)\n}}\n");
    assert_eq!(lowered_with_callees(&effect), source_body(&source, "f", 2));
}

#[test]
fn an_if_without_else_of_an_assignment_lowers_as_the_source_does() {
    let source = "fun f(a: Boolean, b: Int) {\n var x = b\n if (a) x = 2\n}\n";
    // The assignment bare, and in a block of its own.
    for braced in [false, true] {
        let demo = demo(&[("f", &[T::Boolean, T::Int], T::Unit)], move |_, body| {
            let b = body.param(1);
            let (declaration, x) = body.var(T::Int, b);
            let two = body.int(2);
            let assignment = body.set(&x, two);
            let KlibIrStatement::Expression(assignment) = assignment else {
                unreachable!("an assignment is an expression statement");
            };
            let branch = if braced {
                body.block(vec![KlibIrStatement::Expression(assignment)], T::Unit)
            } else {
                assignment
            };
            let a = body.param(0);
            let conditional = body.branches("IF", vec![(a, branch)], T::Unit);
            Some(vec![declaration, KlibIrStatement::Expression(conditional)])
        });
        assert_eq!(
            lowered_f(&demo),
            source_body(source, "f", 2),
            "braced: {braced}"
        );
    }
}

#[test]
fn an_if_chain_without_else_lowers_as_the_source_chain_does() {
    let demo = demo(
        &[("f", &[T::Boolean, T::Boolean, T::Int], T::Int)],
        |_, body| {
            let a = body.param(0);
            let one = body.int(1);
            let first = body.return_(one);
            let b = body.param(1);
            let two = body.int(2);
            let second = body.return_(two);
            let chain = body.branches("IF", vec![(a, first), (b, second)], T::Unit);
            let c = body.param(2);
            Some(vec![KlibIrStatement::Expression(chain), body.ret(c)])
        },
    );
    let source = "fun f(a: Boolean, b: Boolean, c: Int): Int {\n \
                  if (a) return 1 else if (b) return 2\n return c\n}\n";
    assert_eq!(lowered_f(&demo), source_body(source, "f", 3));
}

#[test]
fn a_when_without_else_lowers_as_the_source_statement_does() {
    let demo = with_callees(|body| {
        let a = body.param(0);
        let b = body.param(1);
        let first = body.call("g", vec![b], T::Int);
        let first = body.coerced(first);
        let b = body.param(1);
        let zero = body.int(0);
        let positive = body.builtin(relation("greater"), vec![b, zero], Some("GT"));
        let b = body.param(1);
        let second = body.call("g", vec![b], T::Int);
        let second = body.coerced(second);
        vec![KlibIrStatement::Expression(body.branches(
            "WHEN",
            vec![(a, first), (positive, second)],
            T::Unit,
        ))]
    });
    let source = format!(
        "{CALLEES_SOURCE}fun f(a: Boolean, b: Int) {{\n when {{\n  a -> g(b)\n  \
         b > 0 -> g(b)\n }}\n}}\n"
    );
    let lowered = lowered_with_callees(&demo);
    assert_eq!(lowered, source_body(&source, "f", 2));
    assert_eq!(
        lowered,
        "scope {scope { => when[v0!: Boolean -> call g(v1!: Int): Int, \
         Gt(v1!: Int, Int(0): Int): Boolean -> call g(v1!: Int): Int]: Unit}: Unit}"
    );
}

#[test]
fn a_valued_when_without_else_declines() {
    let demo = demo(&[("f", &[T::Boolean], T::Int)], |_, body| {
        let a = body.param(0);
        let one = body.int(1);
        let when = body.branches("WHEN", vec![(a, one)], T::Int);
        Some(vec![body.ret(when)])
    });
    assert_eq!(
        demo.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         a `when` without an `else` is not typed `Unit`)"
    );
}

// --- Blocks and `Unit` bodies -------------------------------------------------------------------

#[test]
fn a_block_branch_lowers_as_the_source_block_does() {
    let demo = demo(&[("f", &[T::Boolean, T::Int], T::Int)], |_, body| {
        let b = body.param(1);
        let (declaration, y) = body.val(T::Int, b);
        let read = body.get(&y, T::Int);
        let block = body.block(vec![declaration, KlibIrStatement::Expression(read)], T::Int);
        let a = body.param(0);
        let otherwise = body.otherwise();
        let zero = body.int(0);
        let conditional = body.branches("IF", vec![(a, block), (otherwise, zero)], T::Int);
        Some(vec![body.ret(conditional)])
    });
    let source = "fun f(a: Boolean, b: Int): Int {\n return if (a) { val y = b; y } else 0\n}\n";
    let lowered = lowered_f(&demo);
    assert_eq!(lowered, source_body(source, "f", 2));
    assert_eq!(
        lowered,
        "scope {scope {return@Some(0) when[v0!: Boolean -> {val v2: Int = v1!: Int => v2!: Int}: \
         Int, else -> Int(0): Int]: Int: Nothing}: Nothing}"
    );
}

#[test]
fn a_returning_block_is_typed_by_its_jump() {
    // A KLIB types the braced branch of a `Unit` `if` `Unit`; the source block ends in a jump.
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let zero = body.int(0);
        let positive = body.builtin(relation("greater"), vec![a, zero], Some("GT"));
        let a = body.param(0);
        let returned = body.ret(a);
        let block = body.block(vec![returned], T::Unit);
        let guard = body.branches("IF", vec![(positive, block)], T::Unit);
        let zero = body.int(0);
        Some(vec![KlibIrStatement::Expression(guard), body.ret(zero)])
    });
    let source = "fun f(a: Int): Int {\n if (a > 0) {\n  return a\n }\n return 0\n}\n";
    assert_eq!(lowered_f(&demo), source_body(source, "f", 1));
}

#[test]
fn unit_bodies_without_a_return_lower_as_their_source_does() {
    let empty = demo(&[("f", &[T::Int], T::Unit)], |_, _| Some(Vec::new()));
    assert_eq!(lowered_f(&empty), "scope {scope {}: Unit}");
    assert_eq!(
        lowered_f(&empty),
        source_body("fun f(a: Int) {\n}\n", "f", 1)
    );
    let assigning = demo(&[("f", &[T::Int], T::Unit)], |_, body| {
        let a = body.param(0);
        let (declaration, x) = body.var(T::Int, a);
        let one = body.int(1);
        Some(vec![declaration, body.set(&x, one)])
    });
    assert_eq!(
        lowered_f(&assigning),
        source_body("fun f(a: Int) {\n var x = a\n x = 1\n}\n", "f", 1)
    );
    // A trailing call of another type is the source block's value, without a coercion.
    let calling = with_callees(|body| {
        let b = body.param(1);
        let call = body.call("g", vec![b], T::Int);
        vec![KlibIrStatement::Expression(body.coerced(call))]
    });
    let source = format!("{CALLEES_SOURCE}fun f(a: Boolean, b: Int) {{\n g(b)\n}}\n");
    let lowered = lowered_with_callees(&calling);
    assert_eq!(lowered, source_body(&source, "f", 2));
    assert_eq!(lowered, "scope {scope { => call g(v1!: Int): Int}: Int}");
}

#[test]
fn a_valued_body_that_can_end_declines() {
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        Some(vec![KlibIrStatement::Expression(a)])
    });
    assert_eq!(
        demo.declined("f", &[]),
        "the KLIB body of `demo.f` (it ends without a `return`)"
    );
}

#[test]
fn a_valued_body_ending_in_an_endless_loop_lowers_as_its_source_does() {
    for post_test in [false, true] {
        let demo = demo(&[("f", &[T::Int], T::Int)], move |_, body| {
            let block = body.block(Vec::new(), T::Unit);
            let forever = body.boolean(true);
            Some(vec![body.loop_(1, forever, block, post_test)])
        });
        let source = if post_test {
            "fun f(a: Int): Int {\n do {\n } while (true)\n}\n"
        } else {
            "fun f(a: Int): Int {\n while (true) {\n }\n}\n"
        };
        assert_eq!(
            lowered(&demo, "f", &[]),
            source_body(source, "f", 1),
            "{post_test}"
        );
    }
}

#[test]
fn a_valued_body_ending_in_a_constant_loop_that_can_break_declines() {
    for post_test in [false, true] {
        let demo = demo(&[("f", &[T::Boolean], T::Int)], move |_, body| {
            let condition = body.param(0);
            let broken = body.break_(1);
            let leave = body.branches("IF", vec![(condition, broken)], T::Unit);
            let loop_body = body.block(vec![KlibIrStatement::Expression(leave)], T::Unit);
            let forever = body.boolean(true);
            Some(vec![body.loop_(1, forever, loop_body, post_test)])
        });
        assert_eq!(
            demo.declined("f", &[]),
            "the KLIB body of `demo.f` (it ends without a `return`)",
            "{post_test}"
        );
    }
}

#[test]
fn a_statement_after_a_jump_declines() {
    let demo = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let a = body.param(0);
        let first = body.ret(a);
        let a = body.param(0);
        Some(vec![first, body.ret(a)])
    });
    assert_eq!(
        demo.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses a statement after a jump)"
    );
}

// --- `throw` ------------------------------------------------------------------------------------

/// The source's `Throwable`, declared where the stdlib declares it, since checked FIR lowering of
/// these sources runs without a library.
const THROWABLE: &str = "package kotlin\nopen class Throwable\n";

#[test]
fn throw_lowers_as_the_source_throw_does() {
    let shapes: [Shape; 3] = [
        ("f", &[T::Throwable], T::Nothing),
        ("g", &[T::Int, T::Throwable], T::Int),
        ("h", &[T::Int, T::Throwable], T::Int),
    ];
    let demo = demo(&shapes, |name, body| match name {
        "f" => {
            let a = body.param(0);
            Some(vec![KlibIrStatement::Expression(body.throw(a))])
        }
        "g" => {
            let a = body.param(0);
            let zero = body.int(0);
            let negative = body.builtin(relation("less"), vec![a, zero], Some("LT"));
            let e = body.param(1);
            let thrown = body.throw(e);
            let guard = body.branches("IF", vec![(negative, thrown)], T::Unit);
            let a = body.param(0);
            Some(vec![KlibIrStatement::Expression(guard), body.ret(a)])
        }
        _ => {
            let a = body.param(0);
            let zero = body.int(0);
            let negative = body.builtin(relation("less"), vec![a, zero], Some("LT"));
            let e = body.param(1);
            let thrown = body.throw(e);
            let otherwise = body.otherwise();
            let a = body.param(0);
            let conditional = body.branches("IF", vec![(negative, thrown), (otherwise, a)], T::Int);
            Some(vec![body.ret(conditional)])
        }
    });
    let source = format!(
        "{THROWABLE}fun f(a: Throwable): Nothing {{\n throw a\n}}\n\
         fun g(a: Int, e: Throwable): Int {{\n if (a < 0) throw e\n return a\n}}\n\
         fun h(a: Int, e: Throwable): Int {{\n return if (a < 0) throw e else a\n}}\n"
    );
    assert_eq!(
        lowered(&demo, "f", &[]),
        "scope {scope { => throw v0!: kotlin/Throwable: Nothing}: Nothing}"
    );
    for (name, arity) in [("f", 1), ("g", 2), ("h", 2)] {
        assert_eq!(
            lowered(&demo, name, &[]),
            source_body(&source, name, arity),
            "{name}"
        );
    }
}

#[test]
fn a_constructor_call_declines_by_its_form() {
    let demo = demo(&[("f", &[T::Throwable], T::Nothing)], |_, body| {
        let KlibIrSignature::Public(signature) = body.function.signature.clone() else {
            unreachable!("a demo function is public");
        };
        // Any constructor: the form declines before its callee is read.
        let constructed = body.expr(
            T::Throwable,
            KlibIrExprKind::ConstructorCall {
                access: KlibIrMemberAccess {
                    symbol: super::public_symbol(KlibIrSymbolKind::Constructor, signature),
                    arguments: KlibIrArguments::Flat(Vec::new()),
                    type_arguments: Vec::new(),
                    origin: None,
                },
                constructor_type_arguments: 0,
            },
        );
        Some(vec![KlibIrStatement::Expression(body.throw(constructed))])
    });
    assert_eq!(
        demo.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses a constructor call)"
    );
}

// --- String templates ---------------------------------------------------------------------------

#[test]
fn string_templates_lower_as_the_source_templates_do() {
    let shapes: [Shape; 3] = [
        ("f", &[T::Int, T::String], T::String),
        ("g", &[T::Int], T::String),
        ("h", &[T::Int, T::Int], T::String),
    ];
    let demo = demo(&shapes, |name, body| {
        let value = match name {
            "f" => {
                let x = body.string("x");
                let a = body.param(0);
                let y = body.string("y");
                let b = body.param(1);
                body.concat(vec![x, a, y, b])
            }
            "g" => {
                let a = body.param(0);
                body.concat(vec![a])
            }
            // Adjacent literal text is one constant part.
            _ => {
                let first = body.string("a");
                let second = body.string("b");
                let a = body.param(0);
                body.concat(vec![first, second, a])
            }
        };
        Some(vec![body.ret(value)])
    });
    let source = "fun f(a: Int, b: String): String {\n return \"x${a}y$b\"\n}\n\
                  fun g(a: Int): String {\n return \"$a\"\n}\n\
                  fun h(a: Int, b: Int): String {\n return \"a\" + \"b$a\"\n}\n";
    assert_eq!(
        lowered(&demo, "f", &[]),
        "scope {scope {return@Some(0) concat[String(\"x\"), v0!: Int, String(\"y\"), \
         v1!: String]: String: Nothing}: Nothing}"
    );
    for (name, arity) in [("f", 2), ("g", 1), ("h", 2)] {
        assert_eq!(
            lowered(&demo, name, &[]),
            source_body(source, name, arity),
            "{name}"
        );
    }
}

#[test]
fn a_string_constant_lowers_as_the_source_constant_does() {
    let demo = demo(&[("f", &[T::String], T::String)], |_, body| {
        let value = body.string("abc");
        Some(vec![body.ret(value)])
    });
    assert_eq!(
        lowered_f(&demo),
        source_body("fun f(a: String): String {\n return \"abc\"\n}\n", "f", 1)
    );
}

#[test]
fn a_string_template_checked_fir_folds_declines() {
    let demo = demo(&[("f", &[T::Int], T::String)], |_, body| {
        let one = body.int(1);
        let a = body.param(0);
        let value = body.concat(vec![one, a]);
        Some(vec![body.ret(value)])
    });
    assert_eq!(
        demo.declined("f", &[]),
        "the KLIB body of `demo.f` (it uses a string template with a constant of another type)"
    );
}

// --- Loops --------------------------------------------------------------------------------------

/// `x = x - 1`, through the stdlib's `Int.minus`-free form: `x = x + -1` is not source, so the
/// loops decrement through a dependency function `dec`.
fn decrement(body: &mut Body, x: &crate::metadata::klib_ir::KlibIrSymbol) -> KlibIrStatement {
    let read = body.get(x, T::Int);
    let decremented = body.call("dec", vec![read], T::Int);
    body.set(x, decremented)
}

/// `x > bound`.
fn above(body: &mut Body, x: &crate::metadata::klib_ir::KlibIrSymbol, bound: i32) -> KlibIrExprId {
    let read = body.get(x, T::Int);
    let bound = body.int(bound);
    body.builtin(relation("greater"), vec![read, bound], Some("GT"))
}

fn looping(
    build: fn(&mut Body, &crate::metadata::klib_ir::KlibIrSymbol) -> KlibIrStatement,
) -> Demo {
    let shapes: [Shape; 2] = [("dec", &[T::Int], T::Int), ("f", &[T::Int], T::Int)];
    demo(&shapes, move |name, body| {
        if name == "dec" {
            let a = body.param(0);
            return Some(vec![body.ret(a)]);
        }
        let a = body.param(0);
        let (declaration, x) = body.var(T::Int, a);
        let looped = build(body, &x);
        let read = body.get(&x, T::Int);
        Some(vec![declaration, looped, body.ret(read)])
    })
}

const DEC_SOURCE: &str = "fun dec(a: Int): Int {\n return a\n}\n";

fn lowered_loop(demo: &Demo) -> String {
    lowered(demo, "f", &["dec"])
}

#[test]
fn while_loops_lower_as_the_source_loops_do() {
    for post_test in [false, true] {
        let demo = if post_test {
            looping(|body, x| {
                let decrement = decrement(body, x);
                let block = body.block(vec![decrement], T::Unit);
                let condition = above(body, x, 0);
                body.loop_(1, condition, block, true)
            })
        } else {
            looping(|body, x| {
                let decrement = decrement(body, x);
                let block = body.block(vec![decrement], T::Unit);
                let condition = above(body, x, 0);
                body.loop_(1, condition, block, false)
            })
        };
        let source = if post_test {
            format!(
                "{DEC_SOURCE}fun f(a: Int): Int {{\n var x = a\n do {{\n  x = dec(x)\n }} \
                 while (x > 0)\n return x\n}}\n"
            )
        } else {
            format!(
                "{DEC_SOURCE}fun f(a: Int): Int {{\n var x = a\n while (x > 0) {{\n  \
                 x = dec(x)\n }}\n return x\n}}\n"
            )
        };
        assert_eq!(
            lowered_loop(&demo),
            source_body(&source, "f", 1),
            "{post_test}"
        );
    }
    let demo = looping(|body, x| {
        let decrement = decrement(body, x);
        let block = body.block(vec![decrement], T::Unit);
        let condition = above(body, x, 0);
        body.loop_(1, condition, block, false)
    });
    assert_eq!(
        lowered_loop(&demo),
        "scope {scope {val v1: Int = v0!: Int; while@L0(Gt(v1~: Int, Int(0): Int): Boolean) \
         {v1 = call dec(v1~: Int): Int: Unit}: Unit; return@Some(0) v1~: Int: Nothing}: Nothing}"
    );
}

#[test]
fn break_and_continue_lower_as_the_source_jumps_do() {
    // while (true) { if (x > 10) break; if (x < 0) continue; x = dec(x) }
    let demo = looping(|body, x| {
        let over = above(body, x, 10);
        let broken = body.break_(7);
        let leave = body.block(vec![KlibIrStatement::Expression(broken)], T::Unit);
        let leave = body.branches("IF", vec![(over, leave)], T::Unit);
        let read = body.get(x, T::Int);
        let zero = body.int(0);
        let under = body.builtin(relation("less"), vec![read, zero], Some("LT"));
        let resumed = body.continue_(7);
        let resume = body.branches("IF", vec![(under, resumed)], T::Unit);
        let decrement = decrement(body, x);
        let block = body.block(
            vec![
                KlibIrStatement::Expression(leave),
                KlibIrStatement::Expression(resume),
                decrement,
            ],
            T::Unit,
        );
        let forever = body.boolean(true);
        body.loop_(7, forever, block, false)
    });
    let source = format!(
        "{DEC_SOURCE}fun f(a: Int): Int {{\n var x = a\n while (true) {{\n  \
         if (x > 10) {{ break }}\n  if (x < 0) continue\n  x = dec(x)\n }}\n return x\n}}\n"
    );
    assert_eq!(lowered_loop(&demo), source_body(&source, "f", 1));
}

#[test]
fn a_jump_out_of_an_outer_loop_names_that_loop() {
    // outer@ while (x > 0) { while (x > 5) { break@outer }; x = dec(x) }
    let demo = looping(|body, x| {
        let broken = body.break_(1);
        let inner_body = body.block(vec![KlibIrStatement::Expression(broken)], T::Unit);
        let inner_condition = above(body, x, 5);
        let inner = body.loop_(2, inner_condition, inner_body, false);
        let decrement = decrement(body, x);
        let outer_body = body.block(vec![inner, decrement], T::Unit);
        let outer_condition = above(body, x, 0);
        body.loop_(1, outer_condition, outer_body, false)
    });
    let source = format!(
        "{DEC_SOURCE}fun f(a: Int): Int {{\n var x = a\n outer@ while (x > 0) {{\n  \
         while (x > 5) {{ break@outer }}\n  x = dec(x)\n }}\n return x\n}}\n"
    );
    let lowered = lowered_loop(&demo);
    assert_eq!(lowered, source_body(&source, "f", 1));
    assert_eq!(
        lowered,
        "scope {scope {val v1: Int = v0!: Int; while@L0(Gt(v1~: Int, Int(0): Int): Boolean) \
         {while@L1(Gt(v1~: Int, Int(5): Int): Boolean) {break@L0: Nothing}: Nothing; \
         v1 = call dec(v1~: Int): Int: Unit}: Unit; return@Some(0) v1~: Int: Nothing}: Nothing}"
    );
}

#[test]
fn a_loop_body_with_a_trailing_value_lowers_as_the_source_body_does() {
    let demo = looping(|body, x| {
        let decrement = decrement(body, x);
        let read = body.get(x, T::Int);
        let called = body.call("dec", vec![read], T::Int);
        let called = body.coerced(called);
        let block = body.block(
            vec![decrement, KlibIrStatement::Expression(called)],
            T::Unit,
        );
        let condition = above(body, x, 0);
        body.loop_(1, condition, block, false)
    });
    let source = format!(
        "{DEC_SOURCE}fun f(a: Int): Int {{\n var x = a\n while (x > 0) {{\n  x = dec(x)\n  \
         dec(x)\n }}\n return x\n}}\n"
    );
    assert_eq!(lowered_loop(&demo), source_body(&source, "f", 1));
}

#[test]
fn a_jump_naming_no_enclosing_loop_declines() {
    let demo = demo(&[("f", &[T::Int], T::Unit)], |_, body| {
        let broken = body.break_(3);
        Some(vec![KlibIrStatement::Expression(broken)])
    });
    assert_eq!(
        demo.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: \
         a `break` or `continue` names no enclosing loop)"
    );
}

// --- Type operators -----------------------------------------------------------------------------

fn type_check(operator: KlibIrTypeOperator, target: T, result: T) -> Demo {
    demo(&[("f", &[T::NullableAny], result)], move |_, body| {
        let a = body.param(0);
        // An `is` check is typed `Boolean`; its type operand is the type tested.
        let operand = body.ty(target);
        let checked = body.expr(
            result,
            KlibIrExprKind::TypeOperator {
                operator,
                operand,
                argument: a,
            },
        );
        Some(vec![body.ret(checked)])
    })
}

#[test]
fn type_checks_and_casts_lower_as_the_source_operators_do() {
    let cases = [
        (
            KlibIrTypeOperator::InstanceOf,
            T::String,
            T::Boolean,
            "Boolean",
            "a is String",
        ),
        (
            KlibIrTypeOperator::NotInstanceOf,
            T::String,
            T::Boolean,
            "Boolean",
            "a !is String",
        ),
        (
            KlibIrTypeOperator::Cast,
            T::String,
            T::String,
            "String",
            "a as String",
        ),
        (
            KlibIrTypeOperator::Cast,
            T::NullableString,
            T::NullableString,
            "String?",
            "a as String?",
        ),
        (KlibIrTypeOperator::Cast, T::Int, T::Int, "Int", "a as Int"),
    ];
    for (operator, target, result, result_source, expression) in cases {
        let source = format!("fun f(a: Any?): {result_source} {{\n return {expression}\n}}\n");
        assert_eq!(
            lowered_f(&type_check(operator, target, result)),
            source_body(&source, "f", 1),
            "{expression}"
        );
    }
    assert_eq!(
        lowered_f(&type_check(KlibIrTypeOperator::Cast, T::String, T::String)),
        "scope {scope {return@Some(0) written CastNonNull<String>(v0!: \
         Nullable(Obj(TypeName(\"kotlin/Any\"), []))): String: Nothing}: Nothing}"
    );
}

#[test]
fn type_operators_checked_fir_expands_decline_by_name() {
    let cases = [
        (
            KlibIrTypeOperator::SafeCast,
            T::NullableString,
            T::NullableString,
            "a safe cast",
        ),
        (
            KlibIrTypeOperator::InstanceOf,
            T::NullableString,
            T::Boolean,
            "an `is` check of a nullable type",
        ),
        (
            KlibIrTypeOperator::ImplicitNotNull,
            T::String,
            T::String,
            "an implicit non-null assertion",
        ),
    ];
    for (operator, target, result, reason) in cases {
        assert_eq!(
            type_check(operator, target, result).declined("f", &[]),
            format!("the KLIB body of `demo.f` (it uses {reason})")
        );
    }
}

#[test]
fn a_nullability_smart_cast_lowers_as_the_source_smart_cast_does() {
    let cases: [(&'static [T], T, &str, &str); 2] = [
        (&[T::NullableString], T::String, "String", "\"\""),
        (&[T::NullableInt], T::Int, "Int", "0"),
    ];
    for (params, value, source_type, fallback) in cases {
        let demo = demo(&[("f", params, value)], move |_, body| {
            let a = body.param(0);
            let null = body.constant(T::NullableNothing, KlibIrConstant::Null);
            let equal = body.builtin(super::demo_package::eqeq(), vec![a, null], Some("EXCLEQ"));
            let present = body.builtin(super::demo_package::not(), vec![equal], Some("EXCLEQ"));
            let a = body.param(0);
            let narrowed = body.type_operator(KlibIrTypeOperator::ImplicitCast, a, value);
            let returned = body.return_(narrowed);
            let guard = body.branches("IF", vec![(present, returned)], T::Unit);
            let fallback = match value {
                T::String => body.string(""),
                _ => body.int(0),
            };
            Some(vec![KlibIrStatement::Expression(guard), body.ret(fallback)])
        });
        let source = format!(
            "fun f(a: {source_type}?): {source_type} {{\n if (a != null) return a\n \
             return {fallback}\n}}\n"
        );
        assert_eq!(
            lowered_f(&demo),
            source_body(&source, "f", 1),
            "{source_type}"
        );
    }
}

// --- The Kotlin/Native stdlib -------------------------------------------------------------------

/// The rendered body of the stdlib function `package.name` with this receiver and these value
/// parameters, lowered on its own.
fn lowered_stdlib(package: &str, name: &str, receiver: Option<T>, params: &[T]) -> Option<String> {
    let root = distribution_root()?;
    let (libraries, bodies) = stdlib(&root);
    let params = params
        .iter()
        .map(|param| param.semantic())
        .collect::<Vec<_>>();
    let fact = frozen_function(
        &libraries,
        package,
        name,
        receiver.map(T::semantic),
        &params,
    );
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &bodies,
            &KlibCalleeFacts::default(),
        )
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    Some(lowered_body(&unit, function))
}

/// `kotlin.random.boundsErrorMessage` is one string template over two values.
#[test]
fn stdlib_string_template_lowers_as_its_source_does() {
    let Some(lowered) = lowered_stdlib(
        "kotlin/random",
        "boundsErrorMessage",
        None,
        &[T::Any, T::Any],
    ) else {
        return;
    };
    let source = "fun boundsErrorMessage(from: Any, until: Any): String {\n \
                  return \"Random range is empty: [$from, $until).\"\n}\n";
    assert_eq!(lowered, source_body(source, "boundsErrorMessage", 2));
}

/// `kotlin.test.messagePrefix` chooses between a constant and a template by a `null` check.
#[test]
fn stdlib_null_checked_template_lowers_as_its_source_does() {
    let Some(lowered) = lowered_stdlib("kotlin/test", "messagePrefix", None, &[T::NullableString])
    else {
        return;
    };
    let source = "fun messagePrefix(message: String?): String {\n \
                  return if (message == null) \"\" else \"$message. \"\n}\n";
    assert_eq!(lowered, source_body(source, "messagePrefix", 1));
}

/// `kotlin.native.concurrent.ensureNeverFrozen` and `kotlin.native.initRuntimeIfNeeded` are
/// `Unit` bodies without a `return`.
#[test]
fn stdlib_unit_bodies_lower_as_their_source_does() {
    let Some(ensure) = lowered_stdlib(
        "kotlin/native/concurrent",
        "ensureNeverFrozen",
        Some(T::Any),
        &[],
    ) else {
        return;
    };
    assert_eq!(
        ensure,
        source_body("fun Any.ensureNeverFrozen() {\n}\n", "ensureNeverFrozen", 1)
    );
    let initialize = lowered_stdlib("kotlin/native", "initRuntimeIfNeeded", None, &[])
        .expect("the distribution is set");
    assert_eq!(
        initialize,
        source_body("fun initRuntimeIfNeeded() {\n}\n", "initRuntimeIfNeeded", 0)
    );
}
