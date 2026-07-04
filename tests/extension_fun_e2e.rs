//! Top-level extension functions `fun Recv.name(…)` — compiled as static methods whose first
//! parameter is the receiver (Kotlin's strategy). Same-named extensions on different receivers don't
//! collide (dispatched by receiver). A user `operator fun` extension overrides the builtin operator.
//! Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn extension_functions_run() {
    let src = "fun Int.dbl(): Int = this * 2\n\
fun String.dbl(): String = this + this\n\
fun Int.plusX(x: Int): Int = this + x\n\
fun box(): String {\n\
if (3.dbl() != 6) return \"f1\"\n\
if (\"a\".dbl() != \"aa\") return \"f2\"\n\
if (3.plusX(4) != 7) return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "D");
}
