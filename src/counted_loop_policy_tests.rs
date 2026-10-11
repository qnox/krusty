//! Each backend's counted-loop policy over the same lowered IR. Header inlining is a target
//! policy: only a target that asks for it (the JVM) gets the nodes realizing the calls kotlinc
//! inlines into an unsigned loop's header, so the realizer leaves them out of every other target's
//! IR.

use crate::backend::counted_loops::{realize, CountedLoopPolicy};
use crate::fir::SyntheticOriginKind;
use crate::ir::{IrExpr, IrFile, IrNodeOrigin, IrTypeOp};

/// A non-constant `UInt` range, a stepped one, and a `ULong` one bounded by an `until`.
const SOURCE: &str = "fun sink(x: UInt) {}\n\
    fun sinkLong(x: ULong) {}\n\
    fun closed(m: UInt, n: UInt) { for (i in m..n) { sink(i) } }\n\
    fun stepped(m: UInt, n: UInt) { for (i in m..n step 2) { sink(i) } }\n\
    fun longs(m: ULong, n: ULong) { for (i in m until n) { sinkLong(i) } }\n";

/// The checked IR of [`SOURCE`], realized under a target's `policy`.
fn realized(policy: CountedLoopPolicy) -> IrFile {
    realized_source(SOURCE, "UnsignedLoops", policy)
}

/// The checked IR of `source`, realized under a target's `policy`.
fn realized_source(source: &str, stem: &str, policy: CountedLoopPolicy) -> IrFile {
    let platform = Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(crate::toolchain::jvm_classpath_jars_for(
                "// WITH_STDLIB",
            )),
        ))
        .expect("JVM provider initialization"),
    );
    let mut ir = crate::fir_lower::tests::lower_single_source_with_platform(source, stem, platform);
    realize(&mut ir, policy);
    ir
}

/// How many nodes are representation coercions, and how many realize a call kotlinc inlines.
fn counts(ir: &IrFile) -> (usize, usize) {
    let coercions = ir
        .exprs
        .iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    ..
                }
            )
        })
        .count();
    let inlined = ir
        .fir_origins
        .values()
        .filter(|origin| {
            matches!(
                origin,
                IrNodeOrigin::Synthetic {
                    kind: SyntheticOriginKind::InlinedCall,
                    ..
                }
            )
        })
        .count();
    (coercions, inlined)
}

#[test]
fn only_the_jvm_policy_realizes_the_header_s_inlined_calls() {
    let pre_tested = realized(crate::js::backend::COUNTED_LOOPS);
    let jvm = realized(crate::jvm::COUNTED_LOOPS);
    // Both targets keep the 8 bound conversions the progression functions' signatures already
    // needed before header inlining existed. Only the JVM adds the 7 `toInt()`/`toLong()`
    // conversions of non-constant unsigned bounds, and marks those and the 3 unsigned `compareTo`
    // calls as inlined.
    assert_eq!(counts(&pre_tested), (8, 0));
    assert_eq!(counts(&jvm), (15, 10));
}

/// How many progression values the realized function bodies store: a loop that builds its
/// progression keeps it in one temporary, and a counted one has none.
fn stored_progressions(ir: &IrFile) -> usize {
    let progression = crate::types::Ty::obj("kotlin/ranges/IntProgression");
    let mut stored = 0;
    let mut pending: Vec<_> = ir
        .functions
        .iter()
        .filter_map(|function| function.body)
        .collect();
    while let Some(expression) = pending.pop() {
        if matches!(
            &ir.exprs[expression as usize],
            IrExpr::Variable { ty, .. } if *ty == progression
        ) {
            stored += 1;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    stored
}

#[test]
fn only_a_policy_that_counts_until_steps_skips_building_the_progression() {
    // kotlinc builds `0 until 16 step 2` as a progression and reads its bounds, and the JVM must
    // match that. Native counts the same loop from `0..15` instead, so no progression is stored.
    let source = "fun sink(x: Int) {}\n\
        fun stepped() { for (i in 0 until 16 step 2) { sink(i) } }\n";
    let jvm = realized_source(source, "UntilSteps", crate::jvm::COUNTED_LOOPS);
    let native = realized_source(source, "UntilSteps", crate::native::COUNTED_LOOPS);
    assert_eq!(stored_progressions(&jvm), 1);
    assert_eq!(stored_progressions(&native), 0);
}
