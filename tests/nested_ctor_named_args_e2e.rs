use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn named_args_reorder_on_nested_ctor() {
    const SRC: &str = "class Outer { data class Inner(val a: String, val b: Int) }\n\
fun box(): String {\n\
    val i = Outer.Inner(b = 7, a = \"hi\")\n\
    return if (i.a == \"hi\" && i.b == 7) \"OK\" else \"F:\" + i.a + \"/\" + i.b\n\
}\n";
    assert_eq!(run(SRC).expect("named nested ctor reorder"), "OK");
}

#[test]
fn named_args_reorder_on_nested_sealed_subclass() {
    const SRC: &str = "sealed class Event {\n\
    data class Tick(val label: String, val count: Int) : Event()\n\
}\n\
fun box(): String {\n\
    val e = Event.Tick(count = 5, label = \"stop\")\n\
    return if (e.label == \"stop\" && e.count == 5) \"OK\" else \"F:\" + e.label + \"/\" + e.count\n\
}\n";
    assert_eq!(run(SRC).expect("named nested sealed ctor"), "OK");
}
