//! A Java `Class<?>` annotation element is spelled with a Kotlin class literal.
//!
//! `@ExtendWith(SpringExtension::class)`, `@Replaces(X::class)`, `@Import(Config::class)` — the
//! element's own type is `java.lang.Class`, while `Runner::class` has type `KClass`, so presenting
//! the JVM type at the use site rejected every one of them: "actual type is 'reflect.KClass<..>',
//! but 'java.lang.Class!' was expected". kotlinc accepts them all; the emitted VALUE is a class
//! constant either way. The Java generic `Signature` remains semantic: a bounded `Class` element
//! must reject a class literal outside that bound.
use std::fs;
use std::path::{Path, PathBuf};

use super::common;

fn library() -> Option<std::path::PathBuf> {
    let java = [
        (
            "Ext.java".into(),
            "package jl;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME)\n\
         @Target({ElementType.TYPE})\n\
         public @interface Ext {\n\
         \x20   Class<?> one();\n\
         \x20   Class<?>[] many();\n\
         \x20   Class<? extends Runnable> bounded();\n\
         }\n"
            .into(),
        ),
        (
            "One.java".into(),
            "package jl;\n\
             import java.lang.annotation.*;\n\
             @Retention(RetentionPolicy.RUNTIME)\n\
             @Target({ElementType.TYPE, ElementType.ANNOTATION_TYPE})\n\
             public @interface One { Class<?> value(); }\n"
                .into(),
        ),
        (
            "Many.java".into(),
            "package jl;\n\
             import java.lang.annotation.*;\n\
             @Retention(RetentionPolicy.RUNTIME)\n\
             @Target({ElementType.TYPE})\n\
             public @interface Many { Class<?>[] value(); }\n"
                .into(),
        ),
        (
            "Wrap.java".into(),
            "package jl;\n\
             import java.lang.annotation.*;\n\
             @Retention(RetentionPolicy.RUNTIME)\n\
             @Target({ElementType.TYPE})\n\
             public @interface Wrap { One value(); }\n"
                .into(),
        ),
    ];
    common::javac_compile(&java, &[]).map(|(dir, _)| dir)
}

const MAIN: &str = "import jl.*\n\
    class Runner\n\
    @Ext(one = Runner::class, many = arrayOf(Runner::class, Other::class), bounded = Thread::class)\n\
    @One(Runner::class)\n\
    @Many(Runner::class, Other::class)\n\
    @Wrap(One(Runner::class))\n\
    class Tagged\n\
    class Other\n\
    fun box(): String = \"OK\"\n";

fn classpath(library: PathBuf) -> Vec<PathBuf> {
    vec![library, common::stdlib_jar()]
}

fn diagnostic_errors(output: &str) -> Vec<(usize, usize, String)> {
    output
        .lines()
        .filter_map(|rendered| {
            let (location, message) = rendered.split_once("error:")?;
            let location = location.trim().trim_end_matches(':');
            let mut fields = location.rsplitn(3, ':');
            let column = fields.next()?.trim().parse().ok()?;
            let line = fields.next()?.trim().parse().ok()?;
            Some((line, column, message.trim().to_string()))
        })
        .collect()
}

fn normalize_diagnostic_types(message: &str) -> String {
    message
        .replace("reflect.", "")
        .replace("java.lang.", "")
        .replace('!', "")
}

#[test]
fn a_java_class_element_accepts_a_class_literal() {
    let classpath = classpath(library().expect("javac must compile the annotation fixture"));
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &classpath);
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected the fixture: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty rejected a kotlinc-valid fixture: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );

    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(MAIN, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "the accepted fixture must lower and emit: diagnostics={:?}, backend={:?}",
                common::front_end_diagnostics(MAIN, &classpath, Some(jdk.as_path())),
                common::backend_outcome_in_process(MAIN, "Main", &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}

#[test]
fn a_bounded_java_class_element_rejects_an_out_of_bound_literal_like_kotlinc() {
    let classpath = classpath(library().expect("javac must compile the annotation fixture"));
    let source = "import jl.Ext\n\
        class Runner\n\
        class Other\n\
        @Ext(one = Runner::class, many = arrayOf(Other::class), bounded = String::class)\n\
        class Invalid\n";
    let result = common::compiler_diagnostics(&[("Bounded.kt", source)], &classpath);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    let reference = diagnostic_errors(&result.reference_stderr);
    let mut krusty = diagnostic_errors(&result.krusty_stderr);
    krusty.extend(diagnostic_errors(&result.krusty_stdout));
    assert_eq!(
        krusty
            .iter()
            .map(|(line, column, message)| {
                (*line, *column, normalize_diagnostic_types(message))
            })
            .collect::<Vec<_>>(),
        reference
            .iter()
            .map(|(line, column, message)| {
                (*line, *column, normalize_diagnostic_types(message))
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(reference.len(), 1, "expected one bounded-element error");
}

fn compile_both(library: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let work = common::scratch_dir().expect("allocate annotation differential fixture");
    let krusty_dir = work.join("krusty");
    let kotlinc_dir = work.join("kotlinc");
    fs::create_dir_all(&krusty_dir).expect("create krusty output");
    fs::create_dir_all(&kotlinc_dir).expect("create kotlinc output");
    let source = work.join("Main.kt");
    fs::write(&source, MAIN).expect("write source");
    let classpath = classpath(library.to_path_buf());
    let joined = std::env::join_paths(&classpath).expect("join classpath");
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().into_owned(),
        "-cp".to_string(),
        joined.to_string_lossy().into_owned(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().into_owned(),
    ])
    .expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc rejected emission fixture: {stderr}");

    let classes = common::compile_in_process(
        MAIN,
        "Main",
        &classpath,
        Some(common::jdk_modules().as_path()),
    )
    .unwrap_or_else(|| {
        panic!(
            "krusty rejected emission fixture: {:?}",
            common::backend_outcome_in_process(
                MAIN,
                "Main",
                &classpath,
                Some(common::jdk_modules().as_path())
            )
        )
    });
    for (internal, bytes) in classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create class package");
        }
        fs::write(path, bytes).expect("write krusty class");
    }
    (work, krusty_dir, kotlinc_dir)
}

/// The exact `jl.Ext` record from `javap -v`, including its constant-pool indices.
fn ext_annotation_record(directory: &Path) -> Vec<String> {
    let class = directory.join("Tagged.class");
    let dumped = common::javap(&["-v", "-p", &class.to_string_lossy()]).expect("javap unavailable");
    let mut in_visible = false;
    let mut records = Vec::<Vec<String>>::new();
    for line in dumped.lines() {
        let trimmed = line.trim();
        if !in_visible {
            in_visible = trimmed == "RuntimeVisibleAnnotations:";
            continue;
        }
        if !line.chars().next().is_some_and(char::is_whitespace) {
            break;
        }
        let record_header = trimmed.split_once(':').is_some_and(|(index, rest)| {
            !index.is_empty()
                && index.bytes().all(|byte| byte.is_ascii_digit())
                && rest.trim_start().starts_with('#')
        });
        if record_header {
            records.push(Vec::new());
        }
        if let Some(record) = records.last_mut() {
            record.push(trimmed.to_string());
        }
    }
    records
        .into_iter()
        .find(|record| record.iter().any(|line| line.contains("jl.Ext(")))
        .expect("Tagged has jl.Ext")
}

#[test]
fn the_emitted_class_constants_match_kotlinc() {
    let library = library().expect("javac must compile the annotation fixture");
    let (work, krusty_dir, kotlinc_dir) = compile_both(&library);
    let krusty = ext_annotation_record(&krusty_dir);
    let kotlinc = ext_annotation_record(&kotlinc_dir);
    assert_eq!(
        krusty, kotlinc,
        "the complete annotation record and its constant-pool indices must match kotlinc"
    );
    let rendered = krusty.join("\n");
    assert!(rendered.contains("one=class LRunner;"));
    assert!(rendered.contains("many=[class LRunner;,class LOther;]"));
    assert!(rendered.contains("bounded=class Ljava/lang/Thread;"));
    fs::remove_dir_all(work).expect("remove annotation differential fixture");
}
