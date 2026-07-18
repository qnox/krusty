//! Named arguments on a SAME-FILE nested-class constructor (`Outer.Inner(b = 1, a = "x")`). The nested
//! class is hoisted to a top-level decl keyed `Outer.Inner`, so its primary-ctor parameter names map the
//! labels onto positions — exactly like a top-level class ctor. The lowerer realizes the reordering by a
//! source-order temp spill. Round-tripped on the JVM so the reorder is observed, not just compiled.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn named_args_reorder_on_nested_ctor() {
    // Labels supplied OUT OF ORDER (`b` before `a`): the ctor is `(String, Int)`, so a correct reorder
    // stores "hi" into `a` and 7 into `b`.
    const SRC: &str = "class Outer { data class Inner(val a: String, val b: Int) }\n\
fun box(): String {\n\
    val i = Outer.Inner(b = 7, a = \"hi\")\n\
    return if (i.a == \"hi\" && i.b == 7) \"OK\" else \"F:\" + i.a + \"/\" + i.b\n\
}\n";
    assert_eq!(run(SRC).expect("named nested ctor reorder"), "OK");
}

#[test]
fn named_args_reorder_on_nested_sealed_subclass() {
    // A nested sealed subclass constructed with all labels supplied out of order — the reorder maps each
    // label onto its ctor position. (An OMITTED default on a nested ctor is a separate IR-backend gap.)
    const SRC: &str = "sealed class Event {\n\
    data class Tick(val label: String, val count: Int) : Event()\n\
}\n\
fun box(): String {\n\
    val e = Event.Tick(count = 5, label = \"stop\")\n\
    return if (e.label == \"stop\" && e.count == 5) \"OK\" else \"F:\" + e.label + \"/\" + e.count\n\
}\n";
    assert_eq!(run(SRC).expect("named nested sealed ctor"), "OK");
}
