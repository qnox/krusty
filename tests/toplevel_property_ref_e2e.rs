//! Top-level property references `::foo` (a `val` → `KProperty0`) and `::pr` (a `var` →
//! `KMutableProperty0`). Lowered to a `(Mutable)PropertyReference0Impl` singleton whose `get`/`set`
//! dispatch via `invokestatic` to the file-facade accessor; `.name` is inherited from the base.
//! Round-tripped on a real JVM under `-Xverify:all`.

use super::common;

#[test]
fn toplevel_property_refs_run() {
    const SRC: &str = "data class Box(val value: String)\n\
val foo = Box(\"lol\")\n\
var pr = Box(\"first\")\n\
fun box(): String {\n\
    val p = ::foo\n\
    if (p.get() != Box(\"lol\")) return \"Fail value: ${p.get()}\"\n\
    if (p.name != \"foo\") return \"Fail name: ${p.name}\"\n\
    val q = ::pr\n\
    if (q.get() != Box(\"first\")) return \"Fail q: ${q.get()}\"\n\
    if (q.name != \"pr\") return \"Fail qname: ${q.name}\"\n\
    q.set(Box(\"second\"))\n\
    if (q.get().value != \"second\") return \"Fail set: ${q.get()}\"\n\
    return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "PropRefKt");
}
