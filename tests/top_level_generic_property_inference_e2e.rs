//! A top-level property initialized by a GENERIC stdlib function (`val l = listOf(1, 2, 3)`,
//! `val m = mapOf("a" to 1)`) infers its type from the call's ARGUMENTS. The signature-phase property
//! inferrer used a return-agreement probe that can't decide a generic return (every call erases to the
//! same type), so it gave up with "cannot infer the type of property". It now resolves through the same
//! federated `CallResolver` the full checker uses, binding the type parameters from the inferred
//! argument types. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn top_level_list_property_infers_from_args() {
    const SRC: &str = "val l = listOf(1, 2, 3)\n\
        fun box(): String = if (l.size == 3 && l[0] == 1) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("listOf property infers"), "OK");
}

#[test]
fn top_level_map_property_infers_from_args() {
    const SRC: &str = "val m = mapOf(\"a\" to 1, \"b\" to 2)\n\
        fun box(): String = if (m[\"a\"] == 1 && m.size == 2) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("mapOf property infers"), "OK");
}

#[test]
fn top_level_set_property_infers_from_args() {
    const SRC: &str = "val s = setOf(\"x\", \"y\", \"x\")\n\
        fun box(): String = if (s.size == 2 && s.contains(\"x\")) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("setOf property infers"), "OK");
}
