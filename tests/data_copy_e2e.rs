//! Data-class `copy` with named / omitted arguments, realized via the `$default` mechanism: the JVM
//! backend emits a `copy$default(self, fields…, mask, marker)` stub (byte-identical to kotlinc), and a
//! call with omitted args passes a mask. Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn data_class_copy_runs() {
    let src = "data class P(val x: Int, val y: String)\n\
fun box(): String {\n\
val p = P(1, \"a\")\n\
val q = p.copy(y = \"b\")\n\
val r = p.copy(x = 9)\n\
val s = p.copy(2, \"c\")\n\
val t = p.copy()\n\
if (q.x != 1 || q.y != \"b\") return \"f1\"\n\
if (r.x != 9 || r.y != \"a\") return \"f2\"\n\
if (s.x != 2 || s.y != \"c\") return \"f3\"\n\
if (t.x != 1 || t.y != \"a\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "D");
}

#[test]
fn data_class_copy_on_implicit_receiver_runs() {
    // `copy(named = …)` with the IMPLICIT receiver — sugar for `this.copy(named = …)`, so it must bind
    // exactly like the qualified call above. The bare-name path used to resolve the member down to
    // `(params, ret)` and zip the arguments POSITIONALLY, so `copy(resources = x)` type-checked `x`
    // against the FIRST parameter ("inferred type is List but Int was expected"); the lowerer then
    // re-derived the binding itself and bailed because a data class's `copy` defaults are `this.<field>`
    // reads, not constants. Both now go through the one module-member path. Covers a named argument in
    // first, middle and last position.
    let src = "data class D(\n\
val version: Int = 1,\n\
val resources: List<String> = emptyList(),\n\
val tag: String = \"t\",\n\
) {\n\
fun withRes(x: List<String>): D = copy(resources = x)\n\
fun bumped(): D = copy(version = version + 1)\n\
fun retagged(s: String): D = copy(tag = s)\n\
}\n\
fun box(): String {\n\
val a = D(1, emptyList(), \"t\").withRes(listOf(\"r1\"))\n\
if (a.version != 1 || a.resources != listOf(\"r1\") || a.tag != \"t\") return \"f1\"\n\
val b = a.bumped()\n\
if (b.version != 2 || b.resources != listOf(\"r1\") || b.tag != \"t\") return \"f2\"\n\
val c = b.retagged(\"z\")\n\
if (c.version != 2 || c.tag != \"z\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "D");
}
