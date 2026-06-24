//! Enum entry body PROPERTIES: `enum class E { A { val y = … ; override fun f() = y }; … }` — the
//! property becomes a private backing field + getter on the `E$A` subclass, initialized in its
//! constructor after `super(name, ordinal)`. The override reads it as `this.y`. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "E", &[sl], Some(&jdk))
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
