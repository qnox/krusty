//! A top-level delegated property whose delegate's `getValue` is a classpath EXTENSION operator
//! (`operator fun Lazy<T>.getValue(...)` in `LazyKt`, `@InlineOnly`) — `val x by lazy { … }` — now
//! resolves the extension through the classpath extension seam and splices it.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn top_level_val_by_lazy() {
    const SRC: &str = "val computed = StringBuilder()\n\
        val x: String by lazy { computed.append(\"c\"); \"OK\" }\n\
        fun box(): String {\n\
        \x20 val a = x\n\
        \x20 val b = x\n\
        \x20 return if (a == \"OK\" && b == \"OK\" && computed.toString() == \"c\") \"OK\" else \"fail\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("by lazy"), "OK");
}
