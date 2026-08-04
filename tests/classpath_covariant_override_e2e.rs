use super::common;

#[test]
fn named_copy_selects_most_derived_override() {
    const HIERARCHY: &str = "package sample\n\
        open class Parent(open val a: String, open val b: Long, open val c: Long) {\n\
        \x20 open fun copy(a: String, b: Long, c: Long): Parent = Parent(a, b, c)\n\
        }\n\
        class Child(\n\
        \x20 override val a: String,\n\
        \x20 override val b: Long,\n\
        \x20 override val c: Long,\n\
        ) : Parent(a, b, c) {\n\
        \x20 override fun copy(a: String, b: Long, c: Long): Child = Child(a, b, c)\n\
        }\n";
    let main = "package caller\n\
        import sample.Child\n\
        fun update(value: Child, previous: Child?, tick: Long): Child = value.copy(\n\
        \x20 a = previous?.a ?: value.a,\n\
        \x20 b = previous?.b ?: tick,\n\
        \x20 c = tick,\n\
        )\n\
        fun box(): String {\n\
        \x20 val result = update(Child(\"new\", 1, 2), Child(\"old\", 3, 4), 5)\n\
        \x20 return if (result.a == \"old\" && result.b == 3L && result.c == 5L) \"OK\" else \"fail\"\n\
        }\n";
    let Some(output) = common::expect_box_run_against("classpath_copy_override", HIERARCHY, main)
    else {
        return; // toolchain not provisioned
    };
    assert_eq!(output.trim(), "OK");
}
