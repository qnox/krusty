//! The not-null assertion `x!!` — `kotlin/jvm/internal/Intrinsics.checkNotNull` on a duplicate of the
//! value (yields the value, throwing on null). Round-tripped against the JVM under `-Xverify:all`.

use super::common;

#[test]
fn not_null_assert_runs() {
    let src = "fun pick(b: Boolean): String? = if (b) \"hi\" else null\n\
fun len(s: String): Int = s.length\n\
fun box(): String {\n\
val x: String? = pick(true)\n\
if (x!! != \"hi\") return \"f1\"\n\
if (len(pick(true)!!) != 2) return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "N");
}
