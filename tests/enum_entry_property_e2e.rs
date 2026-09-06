//! Enum entry body PROPERTIES: `enum class E { A { val y = … ; override fun f() = y }; … }` — the
//! property becomes a private backing field + getter on the `E$A` subclass, initialized in its
//! constructor after `super(name, ordinal)`. The override reads it as `this.y`. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "E")
}

#[test]
fn entry_property_read_by_override() {
    const SRC: &str =
        "enum class E { A { val y = \"OK\"; override fun f() = y }; abstract fun f(): String }\n\
fun box(): String = E.A.f()\n";
    assert_eq!(run(SRC).expect("entry property compiles + runs"), "OK");
}

#[test]
fn mixed_property_and_method_only_entries() {
    const SRC: &str = "enum class E {\n\
    A { val y = \"O\"; override fun f() = y },\n\
    B { override fun f() = \"K\" };\n\
    abstract fun f(): String\n\
}\n\
fun box(): String = E.A.f() + E.B.f()\n";
    assert_eq!(run(SRC).expect("mixed entries compile + run"), "OK");
}

#[test]
fn int_entry_property() {
    const SRC: &str =
        "enum class E { A { val n = 42; override fun f() = n }; abstract fun f(): Int }\n\
fun box(): String = if (E.A.f() == 42) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("int entry property compiles + runs"), "OK");
}

#[test]
fn entry_property_read_captures_the_exact_entry_receiver_in_a_lambda() {
    const SRC: &str = r#"
enum class E {
    A {
        val suffix = "K"
        val closure = { suffix }
        override val value: String = "O" + closure()
    };
    abstract val value: String
}
fun box(): String = E.A.value
"#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "E"), "OK");
}

#[test]
fn entry_init_local_function_captures_the_exact_entry_receiver() {
    const SRC: &str = r#"
enum class E {
    A {
        val suffix = "K"
        val value: String
        init {
            fun finish() = suffix
            value = "O" + finish()
        }
        override val result: String = value
    };
    abstract val result: String
}
fun box(): String = E.A.result
"#;
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "E"), "OK");
}

#[test]
fn nested_inner_method_publishes_entry_property_read_as_checked_fir() {
    const SRC: &str = r#"
enum class A {
    X {
        val x = "OK"
        inner class Inner { fun foo() = x }
        val z = Inner()
        override val test = z.foo()
    };
    abstract val test: String
}
fun box() = A.X.test
"#;
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&[SRC]),
        Vec::<String>::new(),
    );
}
