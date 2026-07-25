//! Named classpath member calls must preserve ordinary override precedence. The named-argument slot
//! mapper used to score an override and its inherited declaration equally and report an ambiguity.
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
        )\n";
    let Some(lib) = common::compile_lib("classpathcopyoverride", HIERARCHY) else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let cp = vec![lib, stdlib];
    let diagnostics = common::front_end_diagnostics(main, &cp, Some(&jdk));
    assert!(
        diagnostics.is_empty(),
        "override hierarchy produced diagnostics: {diagnostics:?}"
    );
}
