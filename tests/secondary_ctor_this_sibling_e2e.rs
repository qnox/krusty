use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn this_delegation_targets_sibling_secondary() {
    const SRC: &str = "var log: String = \"\"\n\
        class A() {\n\
        \x20 var prop: String = \"\"\n\
        \x20 init { log += \"i\" }\n\
        \x20 constructor(x: String): this() { prop = x; log += \"s\" }\n\
        \x20 constructor(x: Int): this(x.toString()) { prop += \"#int\"; log += \"n\" }\n\
        }\n\
        fun box(): String {\n\
        \x20 val a1 = A(\"abc\")\n\
        \x20 if (a1.prop != \"abc\" || log != \"is\") return \"fail1: ${a1.prop} $log\"\n\
        \x20 log = \"\"\n\
        \x20 val a2 = A(7)\n\
        \x20 if (a2.prop != \"7#int\" || log != \"isn\") return \"fail2: ${a2.prop} $log\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("this-delegation to sibling"), "OK");
}

#[test]
fn named_this_delegation_is_reordered_for_the_primary() {
    const SRC: &str = "class A(val text: String, val number: Int) {\n\
        \x20 constructor(value: Int): this(number = value, text = \"n\")\n\
        }\n\
        fun box(): String {\n\
        \x20 val value = A(7)\n\
        \x20 return value.text + value.number\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("named this delegation"), "n7");
}

#[test]
fn named_this_delegation_keeps_source_evaluation_order() {
    const SRC: &str = "var trace = \"\"\n\
        fun mark(value: String): String { trace += value; return value }\n\
        class A(val a: String, val b: String) {\n\
        \x20 constructor(tag: Int): this(b = mark(\"B\"), a = mark(\"A\"))\n\
        }\n\
        fun box(): String {\n\
        \x20 val value = A(0)\n\
        \x20 return value.a + value.b + \"/\" + trace\n\
        }\n";
    assert_eq!(run(SRC).expect("this delegation evaluation order"), "AB/BA");
}

#[test]
fn this_delegation_supports_primary_defaults_and_varargs() {
    const SRC: &str = "class A(val prefix: String = \"D\", vararg val values: String) {\n\
        \x20 constructor(flag: Boolean): this()\n\
        \x20 constructor(count: Int): this(\"P\", \"X\", \"Y\")\n\
        \x20 constructor(prefix: CharSequence): this(prefix = prefix.toString())\n\
        \x20 fun text(): String = prefix + values.joinToString(\"\")\n\
        }\n\
        fun box(): String {\n\
        \x20 val omitted = A(true).text()\n\
        \x20 val expanded = A(2).text()\n\
        \x20 val empty = A(\"E\" as CharSequence).text()\n\
        \x20 return omitted + \"/\" + expanded + \"/\" + empty\n\
        }\n";
    assert_eq!(
        run(SRC).expect("this delegation defaults and varargs"),
        "D/PXY/E"
    );
}

#[test]
fn constructor_delegation_combines_listed_and_spread_varargs() {
    const SRC: &str = "class Numbers(vararg val values: Int) {\n\
        \x20 constructor(left: IntArray, right: IntArray): this(0, *left, 3, *right, 6)\n\
        }\n\
        class Text(vararg val values: String) {\n\
        \x20 constructor(middle: Array<String>): this(\"a\", *middle, \"d\")\n\
        }\n\
        fun box(): String {\n\
        \x20 val numbers = Numbers(intArrayOf(1, 2), intArrayOf(4, 5))\n\
        \x20 val text = Text(arrayOf(\"b\", \"c\"))\n\
        \x20 return numbers.values.joinToString(\"\") + \"/\" + text.values.joinToString(\"\")\n\
        }\n";
    assert_eq!(run(SRC).expect("mixed constructor varargs"), "0123456/abcd");
}

#[test]
fn constructor_delegation_requires_spread_for_an_array_vararg_argument() {
    const SRC: &str = "class A(vararg val values: Int) {\n\
        \x20 constructor(values: IntArray, marker: Boolean): this(values)\n\
        }\n";
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let diagnostics =
        common::front_end_diagnostics(SRC, &[stdlib], common::jdk_modules().as_deref());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("has no matching target constructor")),
        "expected unspread array delegation diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn constructor_delegation_cycle_is_diagnosed() {
    const SRC: &str = "class A {\n\
        \x20 constructor(value: Int): this(value.toString())\n\
        \x20 constructor(value: String): this(value.isNotEmpty())\n\
        \x20 constructor(value: Boolean): this(if (value) 1 else 0)\n\
        }\n";
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let diagnostics =
        common::front_end_diagnostics(SRC, &[stdlib], common::jdk_modules().as_deref());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("constructor delegation cycle")),
        "expected constructor delegation cycle diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn secondary_constructor_defaults_work_for_calls_and_delegation() {
    const SRC: &str = "var trace = \"\"\n\
        class Token {\n\
        \x20 private constructor(value: String = \"OK\") { trace += value }\n\
        \x20 constructor(code: Int): this()\n\
        \x20 companion object { fun create(): Token = Token() }\n\
        }\n\
        fun box(): String {\n\
        \x20 Token.create()\n\
        \x20 Token(1)\n\
        \x20 return if (trace == \"OKOK\") \"OK\" else trace\n\
        }\n";
    assert_eq!(run(SRC).expect("secondary constructor defaults"), "OK");
}

#[test]
fn branchy_delegation_arguments_keep_this_uninitialized_in_frames() {
    const SRC: &str = "class Label(val text: String) {\n\
        \x20 constructor(ok: Boolean): this(\"O\" + if (ok) \"K\" else \"X\")\n\
        }\n\
        fun box(): String {\n\
        \x20 val yes = Label(true).text\n\
        \x20 val no = Label(false).text\n\
        \x20 return if (yes == \"OK\" && no == \"OX\") \"OK\" else yes + no\n\
        }\n";
    assert_eq!(run(SRC).expect("branchy constructor delegation"), "OK");
}

#[test]
fn ambiguous_this_delegation_is_diagnosed() {
    const SRC: &str = "interface Left\n\
        interface Right\n\
        class Both : Left, Right\n\
        class A() {\n\
        \x20 constructor(value: Left): this()\n\
        \x20 constructor(value: Right): this()\n\
        \x20 constructor(value: Both, tag: Int): this(value)\n\
        }\n";
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let diagnostics =
        common::front_end_diagnostics(SRC, &[stdlib], common::jdk_modules().as_deref());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("ambigu")),
        "expected constructor ambiguity diagnostic, got {diagnostics:?}"
    );
}
