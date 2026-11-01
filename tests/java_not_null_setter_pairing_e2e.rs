//! A Java `setX` pairs with `getX` into a Kotlin `var` when its parameter type equals the getter's,
//! and a platform type `T!` equals both `T` and `T?`. A setter whose parameter is annotated
//! `@NotNull` (an override narrowing an inherited setter, or a class that annotates only one
//! accessor) is still the property's setter; a `@NotNull` getter with a `@Nullable` setter is not,
//! nor is a setter whose parameter differs from the getter only in a type argument. Fixed
//! nullability, projections and arguments compare level by level, through either accessor lookup.

use std::path::PathBuf;

use super::common;

const NOT_NULL: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.PARAMETER, ElementType.TYPE_USE})\n\
    public @interface NotNull { String value() default \"\"; }\n";

const NULLABLE: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.PARAMETER, ElementType.TYPE_USE})\n\
    public @interface Nullable { String value() default \"\"; }\n";

const BASE: &str = "package js;\n\
    import java.util.List;\n\
    public class Base<E> {\n\
        private String name = \"\";\n\
        private List<? super E> sink;\n\
        public String getName() { return name; }\n\
        public void setName(String name) { this.name = name; }\n\
        public List<? super E> getSink() { return sink; }\n\
        public void setSink(List<? super E> sink) { this.sink = sink; }\n\
    }\n";

const SUB: &str = "package js;\n\
    import java.util.List;\n\
    import org.jetbrains.annotations.NotNull;\n\
    public class Sub<E> extends Base<E> {\n\
        @Override public void setName(@NotNull String name) { super.setName(name + \"!\"); }\n\
        @Override public void setSink(@NotNull List<? super E> sink) { super.setSink(sink); }\n\
    }\n";

const ONE_SIDED: &str = "package js;\n\
    import org.jetbrains.annotations.NotNull;\n\
    import org.jetbrains.annotations.Nullable;\n\
    public class OneSided {\n\
        private String label = \"\";\n\
        public String getLabel() { return label; }\n\
        public void setLabel(@NotNull String label) { this.label = label; }\n\
        public @NotNull String getTitle() { return label; }\n\
        public void setTitle(@Nullable String title) { label = title; }\n\
        public java.util.List<String> getItems() { return null; }\n\
        public void setItems(java.util.List<Integer> items) {}\n\
        public @NotNull java.util.List<String> getTags() { return new java.util.ArrayList<>(); }\n\
        public void setTags(@NotNull java.util.List<Integer> tags) {}\n\
    }\n";

const NESTED: &str = "package js;\n\
    import java.util.List;\n\
    import org.jetbrains.annotations.*;\n\
    public class Nested {\n\
        private List<String> a;\n\
        private List<? extends CharSequence> b;\n\
        public @Nullable List<String> getA() { return a; }\n\
        public void setA(@Nullable List<@NotNull String> a) { this.a = a; }\n\
        public List<? extends CharSequence> getB() { return b; }\n\
        public void setB(List<? extends @NotNull CharSequence> b) { this.b = b; }\n\
    }\n";

fn classpath() -> Vec<PathBuf> {
    let sources = [
        ("NotNull.java".to_string(), NOT_NULL.to_string()),
        ("Nullable.java".to_string(), NULLABLE.to_string()),
        ("Base.java".to_string(), BASE.to_string()),
        ("Sub.java".to_string(), SUB.to_string()),
        ("OneSided.java".to_string(), ONE_SIDED.to_string()),
        ("Nested.java".to_string(), NESTED.to_string()),
    ];
    let (library, _) =
        common::javac_compile(&sources, &[]).expect("javac must compile the setter fixture");
    vec![library, common::stdlib_jar()]
}

const USE: &str = "import js.*\n\
fun box(): String {\n\
    val sub = Sub<String>()\n\
    sub.name = \"a\"\n\
    val sink = ArrayList<Any>()\n\
    sub.sink = sink\n\
    val one = OneSided()\n\
    one.label = \"b\"\n\
    if (sub.name != \"a!\") return \"name=${sub.name}\"\n\
    if (sub.sink !== sink) return \"sink\"\n\
    if (one.label != \"b\") return \"label=${one.label}\"\n\
    val nested = Nested()\n\
    nested.a = mutableListOf(\"c\")\n\
    nested.b = listOf(\"d\")\n\
    if (nested.a != listOf(\"c\") || nested.b != listOf(\"d\")) return \"nested\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn a_not_null_java_setter_parameter_still_pairs_with_its_getter_like_kotlinc() {
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
fn a_nullable_setter_of_a_not_null_getter_is_not_its_setter_like_kotlinc() {
    let source = "import js.OneSided\n\
        fun f(o: OneSided) { o.title = \"t\" }\n\
        fun g(o: OneSided) { o.items = listOf(\"x\") }\n\
        fun h(o: OneSided) { o.tags = listOf(\"x\") }\n";
    let result = common::compiler_diagnostics(&[("Invalid.kt", source)], &classpath());
    assert_ne!(result.reference_code, 0, "kotlinc accepted the fixture");
    assert_ne!(result.krusty_code, 0, "krusty accepted the fixture");
    let reference = errors(&result.reference_stderr);
    assert_eq!(
        reference,
        [
            "Invalid.kt:2:24: error: 'val' cannot be reassigned.",
            "Invalid.kt:3:24: error: 'val' cannot be reassigned.",
            "Invalid.kt:4:24: error: 'val' cannot be reassigned.",
        ]
    );
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, reference);
}
