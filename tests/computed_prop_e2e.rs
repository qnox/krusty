//! Computed properties (custom getter, no backing field): top-level `val x get() = …` → static
//! `getX()`; class `val y get() = …` → instance `getX()` (`obj.y`/unqualified `y`). Round-tripped
//! under `-Xverify:all`.

mod common;

#[test]
fn computed_properties_run() {
    const SRC: &str = "val top: Int get() = 42\n\
class C(val a: Int, val b: Int) {\n\
    val sum: Int get() = a + b\n\
    val label: String get() = \"v\" + sum\n\
    fun viaThis(): Int = sum\n\
}\n\
fun box(): String {\n\
if (top != 42) return \"f1\"\n\
val c = C(2, 3)\n\
if (c.sum != 5) return \"f2\"\n\
if (c.viaThis() != 5) return \"f3\"\n\
if (c.label != \"v5\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "P");
}
