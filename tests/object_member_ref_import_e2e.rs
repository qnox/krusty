//! `::foo` where `foo` is imported from a same-file `object` (`import Host.foo`) — a bound reference to
//! the singleton member, lowered like `Host::foo`. Same-file, runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn imported_object_member_reference() {
    const SRC: &str = "import Host.foo\n\
        object Host { fun foo(x: String): String = x + \"K\" }\n\
        fun withO(fn: (String) -> String) = fn(\"O\")\n\
        fun box(): String = withO(::foo)\n";
    assert_eq!(run(SRC).expect("imported object member ref"), "OK");
}

// Vararg-collect adaptation on an imported same-file object member: `::foo` where `foo(vararg x)` is
// imported from `object Host`, adapted to `(String) -> String`.
#[test]
fn imported_object_vararg_collect_adapt() {
    const SRC: &str = "import Host.foo\n\
        fun withO(fn: (String) -> String) = fn(\"O\")\n\
        object Host { fun foo(vararg x: String) = x[0] + \"K\" }\n\
        fun box() = withO(::foo)\n";
    assert_eq!(run(SRC).expect("imported object vararg collect"), "OK");
}
