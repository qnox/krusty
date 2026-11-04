//! A Java `@interface` element reaches Kotlin as a read-only property: `annotation.value`, never
//! `annotation.value()`. Reading one compiles to the element's own interface method, and a `Class`
//! element reads as a `KClass`. An element spelled like a bean getter (`getName()`) is a property
//! of that spelling only; it declares no synthetic `name`.

use std::path::PathBuf;

use super::common;

const INNER_JAVA: &str = "package jl;\n\
public @interface Inner { String value() default \"inner\"; }\n";

const RICH_JAVA: &str = "package jl;\n\
import java.lang.annotation.*;\n\
@Retention(RetentionPolicy.RUNTIME)\n\
public @interface Rich {\n\
    String value() default \"\";\n\
    int[] ids() default {};\n\
    Class<?> type() default Object.class;\n\
    Class<? extends Runnable>[] tasks() default {};\n\
    RetentionPolicy policy() default RetentionPolicy.CLASS;\n\
    Inner nested() default @Inner;\n\
    String[] names() default {};\n\
    long count() default 0L;\n\
    boolean flag() default false;\n\
    String getLabel() default \"label\";\n\
}\n";

fn classpath() -> Vec<PathBuf> {
    let sources = [
        ("Inner.java".to_string(), INNER_JAVA.to_string()),
        ("Rich.java".to_string(), RICH_JAVA.to_string()),
    ];
    let (library, _) =
        common::javac_compile(&sources, &[]).expect("javac must compile the annotation fixture");
    vec![library, common::stdlib_jar()]
}

const USE: &str = "import jl.Rich\n\
class Task : Runnable { override fun run() {} }\n\
@Rich(\"v\", ids = [1, 2], type = String::class, tasks = [Task::class], names = [\"a\"], count = 3, flag = true)\n\
class Tagged\n\
fun describe(a: Rich): String {\n\
    val value: String = a.value\n\
    val ids: IntArray = a.ids\n\
    return value + ids.size + a.type.simpleName + a.tasks.size + a.names.size +\n\
        a.policy.name + a.nested.value + a.count + a.flag + a.getLabel\n\
}\n\
fun box(): String {\n\
    val described = describe(Tagged::class.java.getAnnotation(Rich::class.java))\n\
    return if (described == \"v2String11CLASSinner3truelabel\") \"OK\" else described\n\
}\n";

#[test]
fn a_java_annotation_element_reads_as_a_property_like_kotlinc() {
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
fn a_java_annotation_element_is_neither_a_function_nor_a_bean_getter_like_kotlinc() {
    let source = "import jl.Rich\n\
        fun call(a: Rich): String = a.value()\n\
        fun bean(a: Rich): String = a.label\n";
    let result = common::compiler_diagnostics(&[("Invalid.kt", source)], &classpath());
    assert_ne!(result.reference_code, 0, "kotlinc accepted the fixture");
    assert_ne!(result.krusty_code, 0, "krusty accepted the fixture");
    let reference = errors(&result.reference_stderr);
    assert_eq!(
        reference,
        [
            "Invalid.kt:2:31: error: expression 'value' of type 'String' cannot be invoked as a \
             function. Function 'invoke()' is not found.",
            "Invalid.kt:3:31: error: unresolved reference 'label' on receiver of type 'Rich'.",
        ]
    );
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, reference);
}
