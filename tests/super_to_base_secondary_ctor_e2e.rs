use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn super_targets_a_base_secondary_by_arity() {
    const SRC: &str = "var log = \"\"\n\
        abstract class B {\n\
        \x20 val p: String\n\
        \x20 constructor(a: Int) { p = a.toString(); log += \"b1;\" }\n\
        \x20 constructor(a: Int, b: Int) { p = (a + b).toString(); log += \"b2;\" }\n\
        }\n\
        class A : B {\n\
        \x20 var q: String = \"\"\n\
        \x20 constructor(x: Int, y: Int): super(b = y, a = x) { q = \"two\"; log += \"a2;\" }\n\
        \x20 constructor(x: Int): super(x + 1) { q = \"one\"; log += \"a1;\" }\n\
        \x20 constructor(): this(7) { log += \"a0;\" }\n\
        }\n\
        fun box(): String {\n\
        \x20 val a = A(5, 10)\n\
        \x20 if (a.p != \"15\" || a.q != \"two\") return \"fail1: ${a.p} ${a.q}\"\n\
        \x20 val b = A(3)\n\
        \x20 if (b.p != \"4\" || b.q != \"one\") return \"fail2: ${b.p} ${b.q}\"\n\
        \x20 val c = A()\n\
        \x20 if (c.p != \"8\" || c.q != \"one\") return \"fail3: ${c.p} ${c.q}\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("super to base secondary"), "OK");
}

#[test]
fn super_targets_a_base_with_single_secondary() {
    const SRC: &str = "abstract class B {\n\
        \x20 val p: String\n\
        \x20 constructor(a: Int) { p = \"b\" + a }\n\
        }\n\
        class A : B {\n\
        \x20 constructor(x: Int): super(x)\n\
        }\n\
        fun box(): String {\n\
        \x20 return if (A(5).p == \"b5\") \"OK\" else \"fail\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("super to single base secondary"), "OK");
}

#[test]
fn named_super_delegation_keeps_source_evaluation_order() {
    const SRC: &str = "var trace = \"\"\n\
        fun mark(value: String): String { trace += value; return value }\n\
        open class B(val a: String, val b: String)\n\
        class A : B {\n\
        \x20 constructor(tag: Int): super(b = mark(\"B\"), a = mark(\"A\"))\n\
        }\n\
        fun box(): String {\n\
        \x20 val value = A(0)\n\
        \x20 return value.a + value.b + \"/\" + trace\n\
        }\n";
    assert_eq!(
        run(SRC).expect("super delegation evaluation order"),
        "AB/BA"
    );
}

#[test]
fn super_delegation_supports_primary_defaults_and_varargs() {
    const SRC: &str = "open class B(val prefix: String = \"D\", vararg val values: String) {\n\
        \x20 fun text(): String = prefix + values.joinToString(\"\")\n\
        }\n\
        class A : B {\n\
        \x20 constructor(flag: Boolean): super()\n\
        \x20 constructor(count: Int): super(\"P\", \"X\", \"Y\")\n\
        }\n\
        fun box(): String = A(true).text() + \"/\" + A(2).text()\n";
    assert_eq!(
        run(SRC).expect("super delegation defaults and varargs"),
        "D/PXY"
    );
}

#[test]
fn classpath_super_delegation_supports_multiple_default_masks() {
    let parameters = (0..33)
        .map(|index| format!("val p{index}: Int = {index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let library = format!("package lib\nopen class Base({parameters})\n");
    const MAIN: &str = "import lib.Base\n\
        class Derived : Base { constructor(): super() }\n\
        fun box(): String = if (Derived().p0 == 0 && Derived().p32 == 32) \"OK\" else \"fail\"\n";
    common::expect_box_ok_against("classpath_constructor_multiple_masks", &library, MAIN);
}

#[test]
fn module_super_delegation_recognizes_a_base_from_another_file() {
    // A class without a primary constructor writes its base without parentheses; that syntax is
    // identical to an interface entry until semantic symbols are available. The source-set symbol
    // graph must classify an other-file module class exactly as it classifies a classpath class.
    const BASE: &str = "open class Base(val value: Int)\n";
    const DERIVED: &str = "class Derived : Base {\n\
        \x20 constructor() : super(7)\n\
        }\n\
        fun box(): String { Derived(); return \"OK\" }\n";
    common::expect_front_end_ok_files_with_stdlib(
        &[BASE, DERIVED],
        "module_parenless_base_secondary_super_frontend",
    );
    common::expect_box_ok_files_with_stdlib(
        &[("Base.kt", BASE), ("Derived.kt", DERIVED)],
        "module_parenless_base_secondary_super",
    );
}

#[test]
fn super_delegation_keeps_the_exact_same_arity_overload() {
    const SRC: &str = "open class B {\n\
        \x20 val text: String\n\
        \x20 constructor(value: Int) { text = \"int\" + value }\n\
        \x20 constructor(value: String) { text = \"string\" + value }\n\
        }\n\
        class A : B {\n\
        \x20 constructor(value: Int): super(value)\n\
        }\n\
        fun box(): String = A(7).text\n";
    assert_eq!(run(SRC).expect("same arity super overload"), "int7");
}

#[test]
fn ambiguous_super_delegation_is_diagnosed() {
    const SRC: &str = "interface Left\n\
        interface Right\n\
        class Both : Left, Right\n\
        open class B {\n\
        \x20 constructor(value: Left)\n\
        \x20 constructor(value: Right)\n\
        }\n\
        class A : B {\n\
        \x20 constructor(value: Both): super(value)\n\
        }\n";
    let stdlib = common::stdlib_jar();
    let diagnostics =
        common::front_end_diagnostics(SRC, &[stdlib], Some(common::jdk_modules().as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("ambigu")),
        "expected constructor ambiguity diagnostic, got {diagnostics:?}"
    );
}
