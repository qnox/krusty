//! Splicing a REIFIED stdlib inline (`filterIsInstance<R>()`) whose body lives in the large
//! `CollectionsKt` facade: the `reifiedOperationMarker`'s type-name is pushed by `ldc_w` (0x13, the
//! 2-byte-index form), because that class's constant pool is far past 255 entries. `reify_markers` used
//! to read the name only from `ldc` (0x12, 1-byte), so it saw the marker as malformed and skipped the
//! whole splice — a reified inline compiled in a small class but not against the real stdlib. It now
//! reads the name from `ldc` AND `ldc_w`. This RUNS `filterIsInstance` against the real stdlib (top-level
//! type, nested sealed subclass, and `String`), verifying the reified `instanceof` is substituted and the
//! spliced body verifies and behaves.
use super::common;

#[test]
fn reified_filterisinstance_splice_against_stdlib() {
    let src = "sealed class Shape {\n\
        \x20   class Circle(val r: Int) : Shape()\n\
        \x20   class Square(val s: Int) : Shape()\n\
        }\n\
        fun box(): String {\n\
        \x20   val xs: List<Shape> = listOf(Shape.Circle(1), Shape.Square(2), Shape.Circle(3))\n\
        \x20   val circles = xs.filterIsInstance<Shape.Circle>()\n\
        \x20   val strs = listOf<Any>(\"a\", 1, \"b\").filterIsInstance<String>()\n\
        \x20   return if (circles.size == 2 && strs.size == 2) \"OK\" else \"FAIL:${circles.size}:${strs.size}\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "Main").expect(
            "reified filterIsInstance splice against the real stdlib compiles, verifies, runs"
        ),
        "OK"
    );
}
