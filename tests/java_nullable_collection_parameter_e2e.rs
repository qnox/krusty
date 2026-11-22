//! A `@Nullable` Java parameter of a mapped collection type is `(Mutable)Iterable<*>?`: it keeps the
//! mutability flexibility an unqualified `Iterable<?>` has, so a Kotlin read-only `List` is an
//! applicable argument and the overload beats an `Object` sibling, as in kotlinc. The qualifier is
//! read both as a `TYPE_USE` annotation (Checker Framework) and as a declaration annotation
//! (JetBrains).

use std::path::PathBuf;

use super::common;

const CHECKER_NULLABLE: &str = "package org.checkerframework.checker.nullness.qual;\n\
    import java.lang.annotation.*;\n\
    @Target({ElementType.TYPE_USE, ElementType.TYPE_PARAMETER})\n\
    @Retention(RetentionPolicy.RUNTIME)\n\
    public @interface Nullable {}\n";

const JETBRAINS_NULLABLE: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.PARAMETER, ElementType.TYPE_USE})\n\
    public @interface Nullable { String value() default \"\"; }\n";

fn pick_source(import: &str) -> String {
    format!(
        "import {import};\n\
         import java.util.Map;\n\
         public class Pick {{\n\
         \x20   public static String pick(@Nullable Object value) {{ return \"object\"; }}\n\
         \x20   public static String pick(@Nullable Iterable<?> value) {{ return \"iterable\"; }}\n\
         \x20   public static String pick(@Nullable Map<?, ?> value) {{ return \"map\"; }}\n\
         }}\n"
    )
}

const USE: &str = "fun box(): String {\n\
    if (Pick.pick(listOf(1)) != \"iterable\") return \"list\"\n\
    if (Pick.pick(setOf(\"a\")) != \"iterable\") return \"set\"\n\
    val iterable: Iterable<String> = listOf(\"b\")\n\
    if (Pick.pick(iterable) != \"iterable\") return \"iterable\"\n\
    if (Pick.pick(mapOf(1 to 2)) != \"map\") return \"map\"\n\
    if (Pick.pick(\"text\") != \"object\") return \"object\"\n\
    return \"OK\"\n\
}\n";

fn classpath(annotation: (&str, &str), import: &str) -> Vec<PathBuf> {
    let sources = [
        (annotation.0.to_string(), annotation.1.to_string()),
        ("Pick.java".to_string(), pick_source(import)),
    ];
    let (library, _) =
        common::javac_compile(&sources, &[]).expect("javac must compile the overload fixture");
    vec![library, common::stdlib_jar()]
}

fn assert_picks_the_collection_overload(annotation: (&str, &str), import: &str) {
    let classpath = classpath(annotation, import);
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

#[test]
fn a_type_use_nullable_collection_parameter_accepts_a_read_only_kotlin_collection() {
    assert_picks_the_collection_overload(
        ("Nullable.java", CHECKER_NULLABLE),
        "org.checkerframework.checker.nullness.qual.Nullable",
    );
}

#[test]
fn a_declaration_nullable_collection_parameter_accepts_a_read_only_kotlin_collection() {
    assert_picks_the_collection_overload(
        ("Nullable.java", JETBRAINS_NULLABLE),
        "org.jetbrains.annotations.Nullable",
    );
}
