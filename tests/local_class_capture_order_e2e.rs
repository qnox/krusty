//! A local class takes its captured values in kotlinc's first-use order, not in spelling order:
//! construction code first, then the members in source order, each member's own reads before the
//! closures nested in it. Here the members read `d`, `b`, then `c`, `a`, so the constructor is
//! `($d, $b, $c, $a, p)`.
use super::common;

#[test]
fn local_class_captures_follow_first_use_like_kotlinc() {
    let source = "\
fun capture(a: Int, b: Int, c: Int, d: Int): Int {
    class W(val p: Int) {
        fun run(): Int = d + b + p
        val g: Int get() = c + a
        fun more(): Int = a + d
    }
    val w = W(1)
    return w.run() + w.g + w.more()
}
";
    common::byte_diff_against_kotlinc_cp(
        "CaptureOrder",
        source,
        "CaptureOrderKt$capture$W",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}
