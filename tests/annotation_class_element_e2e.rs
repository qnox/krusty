//! A Java `Class<?>` annotation element is spelled with a Kotlin class literal.
//!
//! `@ExtendWith(SpringExtension::class)`, `@Replaces(X::class)`, `@Import(Config::class)` — the
//! element's own type is `java.lang.Class`, while `Runner::class` has type `KClass`, so presenting
//! the JVM type at the use site rejected every one of them: "actual type is 'reflect.KClass<..>',
//! but 'java.lang.Class!' was expected". kotlinc accepts them all; the emitted VALUE is a class
//! constant either way.
use super::common;

fn library() -> Option<std::path::PathBuf> {
    let java = [
        (
            "Vararg.java".into(),
            "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.TYPE})\n\
         public @interface Vararg { Class<?>[] value(); }\n"
                .into(),
        ),
        (
            "Ext.java".into(),
            "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.TYPE})\n\
         public @interface Ext {\n\
         \x20   Class<?> one();\n\
         \x20   Class<?>[] many();\n\
         }\n"
            .into(),
        ),
    ];
    common::javac_compile(&java, &[]).map(|(dir, _)| dir)
}

const MAIN: &str = "import jl.Ext\n\
    class Runner\n\
    @Ext(one = Runner::class, many = arrayOf(Runner::class, Other::class))\n\
    class Tagged\n\
    class Other\n\
    fun box(): String = \"OK\"\n";

#[test]
fn a_java_class_element_accepts_a_class_literal() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = library().expect("javac must compile the annotation fixture");
    let classpath = vec![library, stdlib.clone()];
    let classes = common::compile_in_process(MAIN, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(MAIN, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}

#[test]
fn the_emitted_class_constants_match_kotlinc() {
    // The payload, not just acceptance: each element is a class constant (`c#`), single and array
    // alike, in the same order kotlinc writes them. The reference strings were captured from
    // kotlinc 2.4.10 by hand — this fixture needs a classpath entry, which the differential
    // helper in `annotation_emission_e2e` cannot take yet.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = library().expect("javac must compile the annotation fixture");
    let classpath = vec![library.clone(), stdlib];
    let Some(classes) = common::compile_in_process(MAIN, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!("krusty rejected the fixture");
    };
    let work = std::env::temp_dir().join(format!("krusty_class_elem_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("scratch dir");
    for (internal, bytes) in &classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, bytes).expect("write class");
    }
    let dumped = common::javap(&["-v", "-cp", &work.to_string_lossy(), "Tagged"])
        .expect("javap unavailable");
    let annotation: Vec<&str> = dumped
        .lines()
        .skip_while(|line| !line.trim_start().starts_with("RuntimeVisibleAnnotations"))
        .take(8)
        .collect();
    let rendered = annotation.join("\n");
    assert!(
        rendered.contains("one=class LRunner;"),
        "single Class element must emit a class constant: {rendered}"
    );
    assert!(
        rendered.contains("many=[class LRunner;,class LOther;]"),
        "array Class element must emit class constants in order: {rendered}"
    );
}

#[test]
fn a_class_vararg_element_takes_bare_class_literals() {
    // The `@ExtendWith(SpringExtension::class)` shape: an array-typed `value` is a POSITIONAL
    // vararg, so its arguments are bare class literals rather than an array. This needs the
    // element type to be Kotlin-facing AND the vararg policy to apply to it.
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = library().expect("javac must compile the annotation fixture");
    let classpath = vec![library, stdlib.clone()];
    const SRC: &str = "import jl.Vararg\n\
        class One\n\
        class Two\n\
        @Vararg(One::class, Two::class)\n\
        class Tagged2\n\
        fun box(): String = \"OK\"\n";
    let classes = common::compile_in_process(SRC, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(SRC, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}
