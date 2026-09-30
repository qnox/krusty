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

#[test]
fn java_util_star_import_keeps_the_kotlin_collection_name() {
    // `import java.util.*` must not rebind `Set` to `java.util.Set`. kotlinc keeps the
    // default-imported `kotlin.collections.Set`, which is a different type from the Java class,
    // so a body that returns `emptySet()` / `this` is well-typed. `Date` still comes from the star.
    const SRC: &str = "import java.util.*\n\
    fun <T> Set<T>.compact(): Set<T> = if (isEmpty()) emptySet() else this\n\
    fun epoch(value: Date): Long = value.time\n\
    fun box(): String {\n\
        val values = setOf(\"O\", \"K\")\n\
        return if (values.compact().size == 2 && epoch(Date(0L)) == 0L) \"OK\" else \"NO\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("java.util.* must not hide kotlin.collections.Set"),
        "OK"
    );
}

#[test]
fn java_util_star_import_resolves_mapped_collection_types() {
    const EXPLICIT: &str = "package first\n\
    import java.util.*\n\
    fun count(values: Collection<String>): Int = values.size\n\
    fun first(values: List<String>): String = values[0]\n";
    const DEFAULT: &str = "package second\n\
    fun last(values: List<String>): String = values[values.size - 1]\n";
    const MAIN: &str = "fun box(): String = \"OK\"\n";
    for sources in [
        [
            ("Explicit.kt", EXPLICIT),
            ("Default.kt", DEFAULT),
            ("Main.kt", MAIN),
        ],
        [
            ("Default.kt", DEFAULT),
            ("Explicit.kt", EXPLICIT),
            ("Main.kt", MAIN),
        ],
    ] {
        assert_eq!(
            common::compile_and_run_files_with_stdlib(&sources)
                .expect("mapped collection imports resolve independently across files"),
            "OK"
        );
    }
}

#[test]
fn files_may_resolve_the_same_simple_name_through_different_imports() {
    const UTIL: &str = "package first\n\
    import java.util.Date\n\
    fun millis(value: Date): Long = value.time\n";
    const SQL: &str = "package second\n\
    import java.sql.Date\n\
    fun millis(value: Date): Long = value.time\n";
    const MAIN: &str = "fun box(): String = \"OK\"\n";

    for sources in [
        [("Util.kt", UTIL), ("Sql.kt", SQL), ("Main.kt", MAIN)],
        [("Sql.kt", SQL), ("Util.kt", UTIL), ("Main.kt", MAIN)],
    ] {
        assert_eq!(
            common::compile_and_run_files_with_stdlib(&sources).expect("imports are file-local"),
            "OK"
        );
    }
}

#[test]
fn one_files_import_does_not_leak_into_another_file() {
    const IMPORTED: &str = "package first\n\
    import java.util.Scanner\n\
    fun read(value: Scanner): String = value.next()\n";
    const UNIMPORTED: &str = "package second\n\
    fun read(value: Scanner): String = value.next()\n";
    const MAIN: &str = "fun box(): String = \"OK\"\n";

    for sources in [
        [
            ("Imported.kt", IMPORTED),
            ("Unimported.kt", UNIMPORTED),
            ("Main.kt", MAIN),
        ],
        [
            ("Unimported.kt", UNIMPORTED),
            ("Imported.kt", IMPORTED),
            ("Main.kt", MAIN),
        ],
    ] {
        assert!(
            common::compile_and_run_files_with_stdlib(&sources).is_none(),
            "an import became visible outside its file"
        );
    }
}

#[test]
fn named_default_classifiers_outrank_a_same_named_star_import() {
    // `kotlin.collections.List` is a simple default import, so `import foo.*` cannot rebind `List`
    // to `foo.List`. `Sequence` is only a default star import, so the explicit star wins.
    // `Iterable.map` from `foo` still outranks the stdlib extension: callable order is unchanged.
    const FOO: &str = "package foo\n\
    class List\n\
    class Sequence {\n\
        fun tag(): String = \"S\"\n\
    }\n\
    fun <T> Iterable<T>.map(x: Int): String = \"M\"\n";
    const USE: &str = "import foo.*\n\
    import java.util.*\n\
    fun lists(values: List<String>): Int = values.size\n\
    fun seq(value: Sequence): String = value.tag()\n\
    fun mapped(values: Iterable<Int>): String = values.map(1)\n\
    fun epoch(value: Date): Long = value.time\n\
    fun box(): String {\n\
        val values = listOf(\"O\")\n\
        return if (lists(values) == 1 && seq(Sequence()) == \"S\" && mapped(listOf(1)) == \"M\" && epoch(Date(0L)) == 0L) \"OK\" else \"NO\"\n\
    }\n";
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[("Foo.kt", FOO), ("Use.kt", USE)])
            .expect("named default classifiers outrank star imports"),
        "OK"
    );
}

#[test]
fn own_package_classifier_outranks_the_named_default() {
    const SRC: &str = "package own\n\
    class List\n\
    fun box(): String {\n\
        val value = List()\n\
        return if (value is List) \"OK\" else \"NO\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("the file's own package outranks kotlin.collections.List"),
        "OK"
    );
}

#[test]
fn a_stdlib_class_in_a_builtin_package_does_not_outrank_a_star_import() {
    // `List` and `IntRange` are `.kotlin_builtins` classifiers, so they stay the Kotlin types.
    // `ArrayList`, `Pair`, and `Exception` live in those same packages as stdlib declarations and
    // are only default stars, so `import foo.*` binds the classes declared in `foo`.
    const FOO: &str = "package foo\n\
    class ArrayList<T> {\n\
        fun tag(): String = \"A\"\n\
    }\n\
    class Pair<A, B> {\n\
        fun tag(): String = \"P\"\n\
    }\n\
    class Exception {\n\
        fun tag(): String = \"E\"\n\
    }\n";
    const USE: &str = "import foo.*\n\
    fun lists(values: List<String>): Int = values.size\n\
    fun range(values: IntRange): Int = values.first\n\
    fun box(): String {\n\
        val values = listOf(\"O\")\n\
        return if (lists(values) == 1 && range(1..1) == 1 && ArrayList<String>().tag() == \"A\" && Pair<Int, Int>().tag() == \"P\" && Exception().tag() == \"E\") \"OK\" else \"NO\"\n\
    }\n";
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[("Foo.kt", FOO), ("Use.kt", USE)])
            .expect("only builtin classifiers outrank an explicit star"),
        "OK"
    );
}

#[test]
fn explicit_java_list_import_binds_the_jvm_class() {
    const SRC: &str = "import java.util.List\n\
    fun takes(values: List<String>): Boolean = values.add(\"a\")\n\
    fun box(): String = \"OK\"\n";
    assert_eq!(
        run(SRC).expect("an explicit import binds java.util.List"),
        "OK"
    );
}

#[test]
fn unqualified_object_stays_java_lang_object() {
    const SRC: &str = "fun hash(value: Object): Int = value.hashCode()\n\
    fun box(): String = if (hash(\"x\") != 0) \"OK\" else \"NO\"\n";
    assert_eq!(
        run(SRC).expect("Object is java.lang.Object, not kotlin.Any"),
        "OK"
    );
}

#[test]
fn map_star_import_binds_the_java_entry() {
    const SRC: &str = "import java.util.Map.*\n\
    fun replace(entry: Entry<String, Int>): Int = entry.setValue(2)\n\
    fun box(): String = \"OK\"\n";
    assert_eq!(
        run(SRC).expect("import java.util.Map.* binds java.util.Map.Entry"),
        "OK"
    );
}

#[test]
fn annotation_star_import_keeps_kotlin_annotation() {
    const SRC: &str = "import java.lang.annotation.*\n\
    fun kind(value: Annotation): kotlin.reflect.KClass<out Annotation> = value.annotationClass\n\
    fun box(): String = \"OK\"\n";
    assert_eq!(
        run(SRC).expect("a java.lang.annotation star does not hide kotlin.Annotation"),
        "OK"
    );
}

#[test]
fn explicit_java_annotation_import_binds_the_jvm_class() {
    const SRC: &str = "import java.lang.annotation.Annotation\n\
    fun jvm(value: Annotation) = value.annotationType()\n\
    fun box(): String = \"OK\"\n";
    assert_eq!(
        run(SRC).expect("an explicit import binds java.lang.annotation.Annotation"),
        "OK"
    );
}

#[test]
fn ambiguous_star_imports_are_rejected_like_kotlinc() {
    // kotlinc rejects a bare `Date` when TWO star-imports both supply it (`java.util.*` AND
    // `java.sql.*`): the name is ambiguous. krusty must also leave it unresolved (a compile error),
    // not silently pick one — the spec's same-level-ambiguity rule.
    const SRC: &str = "import java.util.*\n\
    import java.sql.*\n\
    fun useDate(d: Date): String = \"OK\"\n\
    fun box(): String = \"OK\"\n";
    assert!(
        run(SRC).is_none(),
        "ambiguous star-imported `Date` must be unresolved, matching kotlinc"
    );
}

#[test]
fn explicit_import_resolves_the_star_ambiguity() {
    // …and an explicit import of one of them resolves the ambiguity — an explicit import outranks the
    // star imports, so `Date` binds to `java.util.Date`.
    const SRC: &str = "import java.util.*\n\
    import java.sql.*\n\
    import java.util.Date\n\
    fun useDate(d: Date): String = if (d.time >= 0L) \"OK\" else \"NO\"\n\
    fun box(): String = useDate(Date(0L))\n";
    assert_eq!(
        run(SRC).expect("explicit import outranks the ambiguous star imports"),
        "OK"
    );
}
