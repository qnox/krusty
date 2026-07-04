//! A bare (unqualified, unimported) type name resolves only through Kotlin's default imports — the
//! `kotlin.collections` typealiases (`ArrayList`, `LinkedHashMap`, …, including GENERIC ones whose
//! `@Metadata` lists the type-parameter names before the underlying descriptor) — and NOT to an
//! arbitrary classpath class in a non-default package (which would silently bind `Comparator` to
//! `java.util.Comparator` without an import, the over-match bug). Round-tripped against the JDK.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn generic_collection_typealiases_resolve_bare() {
    // `ArrayList<E>` and `LinkedHashMap<K, V>` are generic `kotlin.collections` typealiases — the
    // decode must skip the type-parameter names (`E`, `K`, `V`) to reach the underlying descriptor.
    const SRC: &str = "fun box(): String {\n\
        val l = ArrayList<String>()\n\
        l.add(\"O\")\n\
        val m = LinkedHashMap<String, String>()\n\
        m[\"k\"] = \"K\"\n\
        return l[0] + m[\"k\"]!!.replace(\"K\", \"K\")\n\
    }\n";
    assert_eq!(
        run(SRC).expect("bare generic collection aliases resolve"),
        "OK"
    );
}

#[test]
fn non_default_package_type_needs_import() {
    // `java.util.Scanner` is NOT in a default-import package and has no `kotlin.*` typealias; a bare
    // reference must be unresolved (kotlinc requires an import), not silently bound to the classpath
    // class. (Used in return position so the unresolved type is a hard error, not a tolerated
    // `: T? = null` annotation.)
    const SRC: &str = "fun makeScanner(): Scanner = TODO()\n\
    fun box(): String = \"OK\"\n";
    assert!(
        run(SRC).is_none(),
        "a bare non-default-package type must NOT resolve without an import"
    );
}

#[test]
fn explicit_import_makes_it_resolve() {
    // …and WITH the import it resolves again — the per-file import path, not a global seed.
    const SRC: &str = "import java.util.Scanner\n\
    fun describe(s: Scanner): String = \"OK\"\n\
    fun box(): String = \"OK\"\n";
    assert_eq!(run(SRC).expect("imported type resolves"), "OK");
}
