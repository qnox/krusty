//! A class implementing a mapped `kotlin.collections` interface must provide the `java.util` method the
//! interface declares for a Kotlin PROPERTY member — `Collection.size` → `size()`, `Map.keys` → `keySet()`,
//! `Map.entries` → `entrySet()`. kotlinc emits each as a synthetic bridge forwarding to the Kotlin getter
//! (`getSize`/`getKeys`); without it the `java.util` abstract stays unimplemented and a call through the
//! interface reference throws `AbstractMethodError` (`java/util/Map.size()I is abstract`).
use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    common::compile_and_run_box(src, "Main", &[sl, jdk.clone()], Some(&jdk))
}

#[test]
fn collection_size_reachable_through_interface() {
    // `c.size` dispatched through the `Collection` reference resolves `java.util.Collection.size()`, which
    // must be the bridge forwarding to `getSize()` — not left abstract.
    const SRC: &str = "class C : Collection<String> {\n\
        \x20   override val size: Int get() = 3\n\
        \x20   override fun isEmpty(): Boolean = false\n\
        \x20   override fun iterator(): Iterator<String> = throw UnsupportedOperationException()\n\
        \x20   override fun containsAll(elements: Collection<String>): Boolean = false\n\
        \x20   override fun contains(element: String): Boolean = false\n\
        }\n\
        fun box(): String {\n\
        \x20   val c: Collection<String> = C()\n\
        \x20   return if (c.size == 3) \"OK\" else \"F:${c.size}\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("Collection.size bridge compiles + runs"),
        "OK"
    );
}
