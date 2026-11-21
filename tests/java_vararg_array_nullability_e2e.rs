//! A qualifier on a Java vararg parameter's array (`Object @Nullable ... values`) does not make the
//! Kotlin `vararg` nullable: kotlinc still accepts element arguments, no argument, and a spread. A
//! qualifier on the elements (`@Nullable Object ... values`) is a distinct, unaffected fact.

use std::path::PathBuf;

use super::common;

const CHECKER_NULLABLE: &str = "package org.checkerframework.checker.nullness.qual;\n\
    import java.lang.annotation.*;\n\
    @Target({ElementType.TYPE_USE, ElementType.TYPE_PARAMETER})\n\
    @Retention(RetentionPolicy.RUNTIME)\n\
    public @interface Nullable {}\n";

const VARARGS: &str = "import org.checkerframework.checker.nullness.qual.Nullable;\n\
    public class Varargs {\n\
    \x20   public static int array(Object @Nullable ... values) { return values == null ? -1 : values.length; }\n\
    \x20   public static int both(@Nullable Object @Nullable ... values) { return values == null ? -1 : values.length; }\n\
    \x20   public static int elements(@Nullable Object ... values) { return values.length; }\n\
    }\n";

const USE: &str = "fun box(): String {\n\
    if (Varargs.array(1.0, 2.0) != 2) return \"array elements\"\n\
    if (Varargs.array() != 0) return \"array empty\"\n\
    if (Varargs.array(*arrayOf<Any?>(1, 2, 3)) != 3) return \"array spread\"\n\
    if (Varargs.both(1.0, null) != 2) return \"both elements\"\n\
    if (Varargs.both() != 0) return \"both empty\"\n\
    if (Varargs.elements(\"a\", null, \"b\") != 3) return \"elements\"\n\
    return \"OK\"\n\
}\n";

fn classpath() -> Vec<PathBuf> {
    let sources = [
        ("Nullable.java".to_string(), CHECKER_NULLABLE.to_string()),
        ("Varargs.java".to_string(), VARARGS.to_string()),
    ];
    let (library, _) =
        common::javac_compile(&sources, &[]).expect("javac must compile the vararg fixture");
    vec![library, common::stdlib_jar()]
}

#[test]
fn a_nullable_java_vararg_array_still_takes_element_arguments_like_kotlinc() {
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
