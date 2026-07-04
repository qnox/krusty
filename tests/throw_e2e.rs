//! `throw` of a classpath exception constructed via `IrExpr::NewExternal` (`new` + `<init>` resolved
//! from the classpath), plus `athrow`. Round-tripped against the JVM under `-Xverify:all`.

use super::common;

#[test]
fn throw_runs() {
    let src = "fun check(b: Boolean): Int { if (b) throw RuntimeException(\"bad\"); return 7 }\n\
fun box(): String {\n\
if (check(false) != 7) return \"f1\"\n\
val e = IllegalStateException(\"unused\")\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "T");
}
