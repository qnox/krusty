//! Which compiler temporaries common lowering materializes. kotlinc's FIR-to-IR and its IR lowerings
//! decide which temporaries exist; every one krusty adds or drops shifts the slots of the locals
//! after it, so these shapes follow kotlinc's rather than an arbitrary snapshot policy.

use super::*;

/// The unnamed declarations lowering materialized whose initializer satisfies `init`.
fn temporaries_initialized_by(ir: &IrFile, init: impl Fn(&IrExpr) -> bool) -> usize {
    ir.exprs
        .iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::Variable {
                    init: Some(value),
                    named: false,
                    ..
                } if init(ir.expr(*value))
            )
        })
        .count()
}

fn is_value_read(expression: &IrExpr) -> bool {
    matches!(expression, IrExpr::GetValue(_))
}

/// A destructuring declaration of an immutable binding reads that binding for every component, as
/// kotlinc does once `JvmOptimizationLowering` has removed the `<destruct>` temporary.
#[test]
fn destructuring_an_immutable_binding_reads_it_for_every_component() {
    let ir = lower_single_source(
        "data class P(val a: String, val b: Int)\n\
         fun parameter(p: P): String { val (a, b) = p; return a + b }\n\
         fun local(): String { val p = P(\"x\", 1); val (a, b) = p; return a + b }\n",
        "DestructureStable",
    );
    assert_eq!(temporaries_initialized_by(&ir, is_value_read), 0);
}

/// The slot of the one local declaration named `name`.
fn named_slot(ir: &IrFile, name: &str) -> u32 {
    let mut slots = ir.value_names.iter().filter_map(|(declaration, declared)| {
        match (declared == name).then(|| ir.expr(*declaration)) {
            Some(IrExpr::Variable { index, .. }) => Some(*index),
            _ => None,
        }
    });
    let slot = slots.next().expect("named local declaration");
    assert!(slots.next().is_none(), "one local named {name}");
    slot
}

/// A mutable binding can change between component calls in general, so its value is kept in a
/// temporary, and so is a container that is not a plain read.
#[test]
fn destructuring_a_mutable_binding_or_a_call_keeps_its_container() {
    let ir = lower_single_source(
        "data class P(val a: String, val b: Int)\n\
         fun make(): P = P(\"x\", 1)\n\
         fun variable(): String { var p = make(); val (a, b) = p; p = make(); return a + b + p }\n",
        "DestructureMutable",
    );
    let container = named_slot(&ir, "p");
    assert_eq!(
        temporaries_initialized_by(
            &ir,
            |init| matches!(init, IrExpr::GetValue(slot) if *slot == container)
        ),
        1
    );

    let ir = lower_single_source(
        "data class P(val a: String, val b: Int)\n\
         fun make(): P = P(\"x\", 1)\n\
         fun call(): String { val (a, b) = make(); return a + b }\n",
        "DestructureCall",
    );
    assert_eq!(
        temporaries_initialized_by(&ir, |init| matches!(
            init,
            IrExpr::Call {
                callee: Callee::Local(_),
                ..
            }
        )),
        1
    );
}

/// Every `when` subject is kotlinc's `tmp_subject`, a read of a parameter or an immutable local
/// included: `JvmOptimizationLowering` keeps a subject temporary initialized from a variable.
#[test]
fn a_when_subject_is_held_even_when_it_reads_an_immutable_binding() {
    let ir = lower_single_source(
        "fun parameter(x: String): Int = when (x) { \"a\" -> 1; \"b\" -> 2; else -> 3 }\n",
        "WhenSubjectParameter",
    );
    assert_eq!(temporaries_initialized_by(&ir, is_value_read), 1);

    let ir = lower_single_source(
        "fun local(p: Int): Int { val x = p + 1; return when (x) { 1 -> 5; 7 -> 8; else -> 3 } }\n",
        "WhenSubjectLocal",
    );
    let subject = named_slot(&ir, "x");
    assert_eq!(
        temporaries_initialized_by(
            &ir,
            |init| matches!(init, IrExpr::GetValue(slot) if *slot == subject)
        ),
        1
    );
}

/// `x as? T` of an immutable binding tests and casts the binding itself (kotlinc's `irLetS` binds an
/// immutable `IrGetValue` without a temporary); a mutable one is evaluated once into a temporary.
#[test]
fn a_safe_cast_of_an_immutable_binding_needs_no_temporary() {
    let ir = lower_single_source(
        "fun parameter(x: Any): Int { val r = x as? String; return r?.length ?: 0 }\n",
        "SafeCastStable",
    );
    let cast_operands = |ir: &IrFile| {
        ir.exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::TypeOp {
                    op: IrTypeOp::Cast,
                    arg,
                    ..
                } => Some(*arg),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let operands = cast_operands(&ir);
    assert_eq!(operands.len(), 1);
    assert!(matches!(ir.expr(operands[0]), IrExpr::GetValue(0)));
    assert_eq!(
        temporaries_initialized_by(&ir, |init| matches!(init, IrExpr::GetValue(0))),
        0
    );

    let ir = lower_single_source(
        "fun variable(y: Any): Int { var x = y; val r = x as? String; x = 1; return r?.length ?: 0 }\n",
        "SafeCastMutable",
    );
    let variable = named_slot(&ir, "x");
    assert_eq!(
        temporaries_initialized_by(
            &ir,
            |init| matches!(init, IrExpr::GetValue(slot) if *slot == variable)
        ),
        1
    );
}

/// What each unnamed declaration of the one lowered `Array(size, init)` block holds, in order;
/// the declarations of the loop body come last.
fn array_constructor_temporaries(ir: &IrFile) -> Vec<&'static str> {
    let block = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Block { stmts, .. }
                if stmts
                    .iter()
                    .any(|statement| matches!(ir.expr(*statement), IrExpr::While { .. })) =>
            {
                Some(stmts.clone())
            }
            _ => None,
        })
        .expect("array constructor loop block");
    let kind = |value: u32| match ir.expr(value) {
        IrExpr::Const(IrConst::Int(0)) => "index",
        IrExpr::NewArray { .. } => "array",
        IrExpr::GetValue(_) => "read",
        _ => "value",
    };
    let mut kinds = Vec::new();
    for statement in &block {
        match ir.expr(*statement) {
            IrExpr::Variable {
                init: Some(value),
                named: false,
                ..
            } => kinds.push(kind(*value)),
            IrExpr::While { body, .. } => {
                let IrExpr::Block { stmts, .. } = ir.expr(*body) else {
                    panic!("array constructor loop body is a block")
                };
                for statement in stmts {
                    if let IrExpr::Variable {
                        init: Some(value),
                        named: false,
                        ..
                    } = ir.expr(*statement)
                    {
                        kinds.push(if kind(*value) == "read" {
                            "element index"
                        } else {
                            "value"
                        });
                    }
                }
            }
            _ => {}
        }
    }
    kinds
}

/// `Array(size) { init }` in kotlinc's `ArrayConstructorLowering` order: the index first, the size
/// only when it is not a constant or a stable read, the array, then the element index inside the
/// loop, onto which the lambda's parameter is remapped (no local of its own) and whose captured
/// stable values it reads in place.
#[test]
fn array_constructor_temporaries_follow_kotlinc_order() {
    let ir = lower_single_source(
        "fun f(n: Int, m: Int): IntArray = IntArray(n) { it * m }\n",
        "ArrayConstructorStableSize",
    );
    assert_eq!(
        array_constructor_temporaries(&ir),
        vec!["index", "array", "element index"]
    );
    assert!(!ir.value_names.values().any(|name| name == "it"));

    let ir = lower_single_source(
        "fun f(n: Int): IntArray = IntArray(n + 1) { i -> i * 2 }\n",
        "ArrayConstructorComputedSize",
    );
    assert_eq!(
        array_constructor_temporaries(&ir),
        vec!["index", "value", "array", "element index"]
    );
    assert!(!ir.value_names.values().any(|name| name == "i"));
}

/// A function value that is not a stable read is evaluated once, after the array is allocated, as
/// kotlinc's `asInlinable` temporary is; a stable one is invoked in place.
#[test]
fn array_constructor_function_value_is_held_after_the_array() {
    let ir = lower_single_source(
        "fun make(): (Int) -> String = { \"x\" }\n\
         fun f(n: Int): Array<String> = Array(n, make())\n",
        "ArrayConstructorFunctionValue",
    );
    assert_eq!(
        array_constructor_temporaries(&ir),
        vec!["index", "array", "value", "element index"]
    );

    let ir = lower_single_source(
        "fun f(n: Int, g: (Int) -> String): Array<String> = Array(n, g)\n",
        "ArrayConstructorStableFunctionValue",
    );
    assert_eq!(
        array_constructor_temporaries(&ir),
        vec!["index", "array", "element index"]
    );
}
