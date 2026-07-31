//! Computed properties (custom getter, no backing field): top-level `val x get() = …` → static
//! `getX()`; class `val y get() = …` → instance `getX()` (`obj.y`/unqualified `y`). Round-tripped
//! under `-Xverify:all`.

use super::common;

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

/// A TOP-LEVEL computed property with NO type annotation infers from its expression getter body —
/// and the body may read another top-level property (`val http get() = httpHolder.client`, the
/// holder idiom). Signature-phase getter inference scoped only context parameters, so the read
/// resolved nothing and the property typed as Error ("cannot infer the type of property 'http'").
/// The initializer branch already threaded the already-collected top-level props in; the getter
/// branch now shares that scope. A computed property reading an EARLIER computed property works
/// too (forward references stay rejected, at parity with initializers).
#[test]
fn computed_property_getter_reads_toplevel_property() {
    const SRC: &str = "class Holder(val client: String)\n\
val httpHolder = Holder(\"OK\")\n\
val http get() = httpHolder.client\n\
val httpAgain get() = http\n\
fun box(): String = if (http == \"OK\" && httpAgain == \"OK\") \"OK\" else \"fail: $http\"\n";
    common::assert_box_ok_with_stdlib(SRC, "G");
}
