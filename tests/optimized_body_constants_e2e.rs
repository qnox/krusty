//! kotlinc optimizes a method after its mandatory stack normalization (FixStack, and the coroutine
//! transformer for a suspend function) and writes the constants the optimized body names into the
//! class's pool. The redundant-boxing pass, for one, rewrites an iteration over an `IntRange` to
//! `IntIterator.nextInt`, a constant the emitted body never named.
//!
//! krusty looked those constants up in the pool as it stood before the passes ran and wrote the
//! normalized body unoptimized when one was missing, so such a method kept its boxed `next()`
//! call, its `nop`s and its redundant jumps.
use super::common;

/// `map` saves the destination collection on the stack around its inlined lambda, so FixStack
/// normalizes the method, and the range's elements are unboxed only by the optimizer.
const MAPPED_RANGE: &str = r#"
fun mapped(): Set<String> = (1..10).map { it.toString() }.toSet()

fun box(): String {
    val values = mapped()
    return if (values.size == 10 && "7" in values) "OK" else "fail: $values"
}
"#;

#[test]
fn a_normalized_body_names_the_constants_its_optimization_adds_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("MappedRange.kt", MAPPED_RANGE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_normalized_body_with_added_constants_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(MAPPED_RANGE, "MappedRange");
}
