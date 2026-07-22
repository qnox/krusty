//! Java-source interop: a box test whose `// FILE:` blocks include `.java` sources (the corpus'
//! `codegen/box` Java-interop shape). The Java files are compiled by the persistent JavaRunner's
//! in-process javac (`common::javac_compile` — no per-test `javac` spawn), their output directory
//! joins krusty's compile classpath (loose-`.class` dir entries are already supported), and the
//! resulting classes run together with krusty's in one BoxRunner classloader.

use super::common;

/// javac_compile alone: one Java class in, its `.class` bytes out.
#[test]
fn javac_compile_returns_class_bytes() {
    let Some(out) = common::javac_compile(
        &[(
            "J.java".to_string(),
            "public class J { public static String ok() { return \"OK\"; } }".to_string(),
        )],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let (dir, classes) = out;
    assert_eq!(classes.len(), 1, "one class expected, got {classes:?}");
    assert_eq!(classes[0].0, "J");
    // Class-file magic.
    assert_eq!(&classes[0].1[..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
    assert!(dir.join("J.class").is_file());
    cleanup(&dir);
}

/// Nested types come back too, named by their relative path stem.
#[test]
fn javac_compile_collects_nested_classes() {
    let Some((_dir, classes)) = common::javac_compile(
        &[(
            "Outer.java".to_string(),
            "public class Outer { public interface Inner { void run(); } }".to_string(),
        )],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let mut names: Vec<&str> = classes.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Outer", "Outer$Inner"]);
    cleanup(&_dir);
}

/// Remove a `javac_compile` scratch tree (the classes dir's parent holds both `src/` and
/// `classes/`).
fn cleanup(classes_dir: &std::path::Path) {
    if let Some(root) = classes_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// The CIRCULAR direction (slice 2, Kotlin-first): Java extends a Kotlin class, Kotlin calls the
/// Java class. Pipeline: signature stubs from the Java source (`krusty::jvm::java_stub`, no
/// javac) → krusty compiles Kotlin against the stub dir → real javac compiles the Java against
/// krusty's output → both class sets run together. The stubs never reach the runtime.
#[test]
fn java_extends_kotlin_via_stub_pipeline() {
    let kotlin = r#"
open class A {
    open fun name(): String = "FAIL:A"
}

fun box(): String = J().name()
"#;
    let java = "public class J extends A { @Override public String name() { return \"OK\"; } }";
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let jars = common::classpath_jars_for(kotlin);

    // 1. Stubs: resolve `A` as a known Kotlin class, everything else via a real Classpath.
    let mut cp_paths = jars.clone();
    cp_paths.push(jdk.clone());
    let classpath = krusty::jvm::classpath::Classpath::new(cp_paths);
    let resolve = |cand: &str| cand == "A" || classpath.find(cand).is_some();
    let stubs =
        krusty::jvm::java_stub::stub_classes(&[("J.java".to_string(), java.to_string())], &resolve)
            .expect("stub generation");

    let root = std::env::temp_dir().join(format!("krusty_stub_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let stubdir = root.join("stubs");
    let kotlindir = root.join("kotlin");
    for (name, bytes) in &stubs {
        let p = stubdir.join(format!("{name}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }

    // 2. Kotlin against the stubs.
    let mut cp = jars.clone();
    cp.push(stubdir);
    let kotlin_classes =
        match common::compile_in_process(kotlin, "MainKt", &cp, Some(jdk.as_path())) {
            Some(c) => c,
            None => {
                let d = common::front_end_diagnostics(kotlin, &cp, Some(jdk.as_path()));
                let _ = std::fs::remove_dir_all(&root);
                panic!("krusty should compile Kotlin against the stub dir; diags: {d:?}");
            }
        };

    // 3. Real javac against krusty's output; the stub dir is NOT on javac's classpath.
    for (name, bytes) in &kotlin_classes {
        let p = kotlindir.join(format!("{name}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    let mut javac_cp = jars.clone();
    javac_cp.push(kotlindir.clone());
    let Some((javadir, java_classes)) =
        common::javac_compile(&[("J.java".to_string(), java.to_string())], &javac_cp)
    else {
        let _ = std::fs::remove_dir_all(&root);
        panic!("javac should compile J against krusty's emitted A");
    };
    cleanup(&javadir);
    let _ = std::fs::remove_dir_all(&root);

    // 4. Run with the REAL classes only.
    let mut classes = kotlin_classes;
    classes.extend(java_classes);
    let box_class = common::find_box_class(&classes).expect("box() class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

/// The `// MODULE:` chaining shape with a Java file in the DEPENDENCY module: `lib` is a Java
/// class plus Kotlin that uses it, `main` is Kotlin `box()` against lib's emitted dir on the
/// classpath — the same javac-first, dir-chaining flow `compile_module_test` performs per module.
#[test]
fn module_dependency_with_java_source() {
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let jars = common::classpath_jars_for("");
    // Module `lib`: J.java + lib.kt (Kotlin wrapping the Java class).
    let Some((javadir, java_classes)) = common::javac_compile(
        &[(
            "J.java".to_string(),
            "public class J { public static String part() { return \"O\"; } }".to_string(),
        )],
        &jars,
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let mut libcp = jars.clone();
    libcp.push(javadir.clone());
    let lib_kotlin = common::compile_in_process(
        "class A { fun part(): String = J.part() + \"K\" }",
        "Lib",
        &libcp,
        Some(jdk.as_path()),
    )
    .expect("lib kotlin compiles against the javac dir");
    // Write lib's FULL output (kotlin + java classes) to one dir — the chained module classpath.
    let root = std::env::temp_dir().join(format!("krusty_modjava_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let libdir = root.join("lib");
    for (name, bytes) in lib_kotlin.iter().chain(java_classes.iter()) {
        let p = libdir.join(format!("{name}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    if let Some(jroot) = javadir.parent() {
        let _ = std::fs::remove_dir_all(jroot);
    }
    // Module `main`: box() against lib's dir.
    let mut maincp = jars.clone();
    maincp.push(libdir);
    let main_classes = common::compile_in_process(
        "fun box(): String = A().part()",
        "MainKt",
        &maincp,
        Some(jdk.as_path()),
    )
    .expect("main compiles against lib's emitted dir");
    let mut classes = lib_kotlin;
    classes.extend(java_classes);
    classes.extend(main_classes);
    let _ = std::fs::remove_dir_all(&root);
    let box_class = common::find_box_class(&classes).expect("box class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

/// Kotlin `box()` calling a static method on a javac-compiled Java class (the
/// `constants/numberLiteralCoercionToInferredType.kt` shape, minus the K2-ignored parts).
#[test]
fn kotlin_calls_java_static() {
    run_mixed(
        &[(
            "J.java",
            "public class J { public static String greet() { return \"OK\"; } }",
        )],
        "fun box(): String = J.greet()",
    );
}

/// Kotlin class extending a javac-compiled Java base and overriding its method (the
/// `fakeOverride/kt40180.kt` shape).
#[test]
fn kotlin_extends_java_base() {
    run_mixed(
        &[(
            "Base.java",
            "public class Base { public String foo(String s) { return \"FAIL:base\"; } }",
        )],
        r#"
class Derived : Base() {
    override fun foo(s: String): String = s
}

fun box(): String = Derived().foo("OK")
"#,
    );
}

/// Compile the Java sources with javac, then the Kotlin source with krusty against the javac output
/// dir on the classpath, and run `box()` with both class sets in one loader. Asserts "OK".
fn run_mixed(java: &[(&str, &str)], kotlin: &str) {
    let java_owned: Vec<(String, String)> = java
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    let Some((javadir, java_classes)) = common::javac_compile(&java_owned, &[]) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        eprintln!("skipping: JDK modules unavailable");
        return;
    };
    // Gate-canonical jars (stdlib/test/annotations — Intrinsics must resolve at runtime); the javac
    // output dir joins only the COMPILE classpath. The run classpath stays jars-only so the pooled
    // BoxRunner JVM (keyed by classpath) is reused across tests — the Java classes ride along as
    // bytes in the in-memory loader.
    let jars: Vec<std::path::PathBuf> = common::classpath_jars_for(kotlin);
    let mut cp = jars.clone();
    cp.push(javadir.clone());
    let mut classes = match common::compile_in_process(kotlin, "MainKt", &cp, Some(jdk.as_path())) {
        Some(c) => c,
        None => {
            let d = common::front_end_diagnostics(kotlin, &cp, Some(jdk.as_path()));
            panic!("krusty should compile Kotlin against the javac output dir; diags: {d:?}");
        }
    };
    // Scratch src+classes tree done — everything needed is in memory now.
    if let Some(root) = javadir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    classes.extend(java_classes);
    let box_class = common::find_box_class(&classes).expect("box() class");
    let got = common::run_box(&classes, &box_class, &jars).expect("box run");
    assert_eq!(got, "OK");
}

/// Expression-position static call on a same-(root-)package class must resolve like the ctor and
/// type positions do (the `imported_type_internal` fallback in the static-receiver path).
#[test]
fn root_package_static_call_matches_other_positions() {
    run_mixed(
        &[(
            "K.java",
            "public class K { public static String s() { return \"OK\"; } }",
        )],
        "fun box(): String = K.s()",
    );
}

/// A top-level VALUE named like the class shadows it in receiver position (Kotlin shadowing —
/// `value_root_shadows_classifier` must keep winning over the classpath fallback): `K.s()` then
/// resolves `s` against the String value and fails, it must NOT silently become the static call.
#[test]
fn value_shadows_classpath_class_in_static_receiver_position() {
    let Some((javadir, _)) = common::javac_compile(
        &[(
            "K.java".to_string(),
            "public class K { public static String s() { return \"FAIL:static\"; } }".to_string(),
        )],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let mut cp = common::classpath_jars_for("");
    cp.push(javadir.clone());
    let src = "val K = \"value\"\nfun box(): String = K.s()";
    let d = common::front_end_diagnostics(src, &cp, Some(jdk.as_path()));
    if let Some(root) = javadir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    assert!(
        !d.is_empty(),
        "value K must shadow class K; expected a resolution error, got clean compile"
    );
}
