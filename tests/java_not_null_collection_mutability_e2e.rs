//! A Java collection annotated `@NotNull` is `(Mutable)List<T!>`, not `MutableList<T!>`: the
//! annotation fixes nullability alone, so a read-only `List` still reaches a Java parameter, a Kotlin
//! override may narrow it to either face, and a result is usable as both. A declaration inferred from
//! one publishes the same flexible type kotlinc writes into its metadata and generic signature,
//! raw (`@NotNull List`) or parameterized.

use std::path::PathBuf;

use super::common;

const NOT_NULL: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.FIELD, ElementType.PARAMETER, \
    ElementType.LOCAL_VARIABLE})\n\
    public @interface NotNull { String value() default \"\"; }\n";

const API: &str = "package jn;\n\
    import java.util.*;\n\
    import org.jetbrains.annotations.NotNull;\n\
    public class Api {\n\
        public static int take(@NotNull List<String> xs) { return xs.size(); }\n\
        public static @NotNull List<String> give() { return new ArrayList<>(Arrays.asList(\"a\")); }\n\
        public static int keys(@NotNull Set<String> keys) { return keys.size(); }\n\
        public static <T> @NotNull List<T> same(@NotNull List<T> xs) { return xs; }\n\
        public static int count(@NotNull Iterable<String> xs) { int n = 0; for (String x : xs) n++; return n; }\n\
        @SuppressWarnings(\"rawtypes\") public static @NotNull List raw() { return new ArrayList(); }\n\
    }\n";

const BASE: &str = "package jn;\n\
    import java.util.List;\n\
    import org.jetbrains.annotations.NotNull;\n\
    public abstract class Base {\n\
        public abstract @NotNull List<String> items();\n\
        public abstract int accept(@NotNull List<String> xs);\n\
    }\n";

const HANDLER: &str = "package jn;\n\
    import java.util.List;\n\
    import org.jetbrains.annotations.NotNull;\n\
    public interface Handler { int handle(@NotNull List<String> xs); }\n";

fn classpath() -> Vec<PathBuf> {
    let sources = [
        ("NotNull.java".to_string(), NOT_NULL.to_string()),
        ("Api.java".to_string(), API.to_string()),
        ("Base.java".to_string(), BASE.to_string()),
        ("Handler.java".to_string(), HANDLER.to_string()),
    ];
    let (library, _) =
        common::javac_compile(&sources, &[]).expect("javac must compile the collection fixture");
    vec![library, common::stdlib_jar()]
}

const USE: &str = "import jn.*\n\
class ReadOnly(private val ro: List<String>) : Base() {\n\
    override fun items(): List<String> = ro\n\
    override fun accept(xs: List<String>): Int = xs.size\n\
}\n\
class Mutable(private val m: MutableList<String>) : Base() {\n\
    override fun items(): MutableList<String> = m\n\
    override fun accept(xs: MutableList<String>): Int = xs.size\n\
}\n\
fun mutableArgument(m: MutableList<String>) = Api.same(m)\n\
fun explicit(ro: List<String>) = Api.same<String>(ro)\n\
fun declared(ro: List<String>): List<String> = Api.same(ro)\n\
fun <T> generic(x: List<T>) = Api.same(x)\n\
val given = Api.give()\n\
fun raw() = Api.raw()\n\
fun box(): String {\n\
    val ro: List<String> = listOf(\"x\")\n\
    val m: MutableList<String> = mutableListOf(\"y\")\n\
    var n = Api.take(ro) + Api.take(m) + Api.keys(setOf(\"k\")) + Api.count(ro)\n\
    val g = Api.give()\n\
    g.add(\"b\")\n\
    val r: List<String> = g\n\
    val mm: MutableList<String> = g\n\
    var v = Api.give()\n\
    v = m\n\
    val same: List<String> = Api.same(ro)\n\
    val h = Handler { xs -> xs.size }\n\
    n += h.handle(ro) + ReadOnly(ro).accept(m) + Mutable(m).items().size + v.size + same.size\n\
    n += r.size + mm.size + generic(ro).size + given.size\n\
    return if (n == 15) \"OK\" else \"n=$n\"\n\
}\n";

#[test]
fn a_not_null_java_collection_accepts_both_faces_like_kotlinc() {
    let classpath = classpath();
    let classes = common::classes_against_kotlinc_module_cp(&[("Use.kt", USE)], &classpath[..1]);
    assert_eq!(classes.differences(), Vec::<String>::new());

    let jdk = common::jdk_modules();
    let compiled = common::compile_in_process(USE, "Use", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "the accepted fixture must lower and emit: {:?}",
                common::front_end_diagnostics(USE, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&compiled, "UseKt", &classpath).expect("box runner"),
        "OK"
    );
}

/// Every `file:line:column: error: message` record, in order.
fn errors(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| line.contains(": error: "))
        .map(|line| {
            let start = line.find("Invalid.kt").expect("an error names its file");
            line[start..].to_string()
        })
        .collect()
}

#[test]
fn a_not_null_java_collection_renders_its_mutability_flexibility_like_kotlinc() {
    let source = "import jn.Api\n\
        fun bad(): Set<String> = Api.give()\n\
        fun absent() = Api.take(null)\n\
        val set: Set<String> = Api.same(listOf(\"x\"))\n";
    let result = common::compiler_diagnostics(&[("Invalid.kt", source)], &classpath());
    assert_ne!(result.reference_code, 0, "kotlinc accepted the fixture");
    assert_ne!(result.krusty_code, 0, "krusty accepted the fixture");
    let reference = errors(&result.reference_stderr);
    assert_eq!(
        reference,
        [
            "Invalid.kt:2:26: error: return type mismatch: expected 'Set<String>', actual \
             '(Mutable)List<String!>'.",
            "Invalid.kt:3:25: error: null cannot be a value of a non-null type \
             '(Mutable)List<String!>'.",
            "Invalid.kt:4:22: error: initializer type mismatch: expected 'Set<String>', actual \
             '(Mutable)List<String!>'.",
        ]
    );
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, reference);
}
