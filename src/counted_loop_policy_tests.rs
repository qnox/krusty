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
    let platform = Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(crate::toolchain::classpath_jars_for(
                "// WITH_STDLIB",
            )),
        ))
        .expect("JVM provider initialization"),
    );
    let mut ir = crate::fir_lower::tests::lower_single_source_with_platform(
        SOURCE,
        "UnsignedLoops",
        platform,
    );
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
