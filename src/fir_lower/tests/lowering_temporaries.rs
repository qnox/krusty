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
