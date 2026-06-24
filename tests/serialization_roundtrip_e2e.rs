//! Real serialization round-trip: krusty compiles `@Serializable Foo` (its plugin emitting the
//! `$serializer`), the REAL kotlin compiler compiles a `box()` driver that does
//! `Json.encodeToString(Foo.serializer(), Foo(1,"x"))` against krusty's classes + the published
//! `kotlinx-serialization` runtime, and the JVM runs it — asserting the JSON krusty's serializer
//! produces is correct. This is the kotlinx.serialization conformance contract (encode), executed.
//!
//! Split compilation (krusty emits the serializer; kotlinc compiles the Json driver) sidesteps the
//! Json-resolution gap and mirrors how the KSP e2e splits responsibilities. Opt-in
//! `KRUSTY_SER_E2E=1`; reuses the `.ksp-toolchain` (kotlin-compiler + JDK 21) and the serialization
//! runtime from the gradle cache. Self-skips if prerequisites are missing.

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;

mod common;

/// Recursively locate a `<prefix>*.jar` (no `-sources`) under a root.
fn walk(dir: &std::path::Path, prefix: &str, depth: usize, out: &mut Option<PathBuf>) {
    if out.is_some() || depth > 10 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, prefix, depth + 1, out);
        } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
            if n.starts_with(prefix) && n.ends_with(".jar") && !n.contains("sources") {
                *out = Some(p.clone());
                return;
            }
        }
    }
}

fn find(prefix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let mut out = None;
    walk(
        &std::path::Path::new(&home).join(".gradle"),
        prefix,
        0,
        &mut out,
    );
    out
}

#[test]
fn serializable_class_encode_round_trips() {
    if std::env::var("KRUSTY_SER_E2E").is_err() {
        eprintln!("skipping: set KRUSTY_SER_E2E=1 (heavy: kotlinc + JDK 21 + runtime)");
        return;
    }
    let tool = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".ksp-toolchain");
    let libs = tool.join("libs");
    let jdk = std::fs::read_dir(&tool).ok().and_then(|rd| {
        rd.flatten().map(|e| e.path()).find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("jdk-21"))
                && p.join("bin/java").exists()
        })
    });
    let (Some(jdk), true) = (jdk, libs.exists()) else {
        eprintln!("skipping: .ksp-toolchain (kotlin-compiler + JDK 21) not provisioned — run the KSP e2e first");
        return;
    };
    let cc: String = std::fs::read_dir(&libs)
        .unwrap()
        .flatten()
        .map(|e| e.path().display().to_string())
        .filter(|p| p.ends_with(".jar"))
        .collect::<Vec<_>>()
        .join(":");
    // stdlib from the toolchain (matches the kotlin-compiler version used to compile the driver).
    let stdlib = std::fs::read_dir(&libs)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("kotlin-stdlib-2") && n.ends_with(".jar"))
        });
    let (Some(core), Some(json), Some(stdlib)) = (
        find("kotlinx-serialization-core-jvm"),
        find("kotlinx-serialization-json-jvm"),
        stdlib,
    ) else {
        eprintln!("skipping: serialization runtime jars not in cache");
        return;
    };
    let jimage = jdk.join("lib/modules");

    // 1. krusty compiles `@Serializable Foo` (plugin emits the $serializer), in-process.
    let classes_dir = {
        use krusty::diag::DiagSink;
        use krusty::ir_lower::lower_file;
        use krusty::jvm::classpath::Classpath;
        use krusty::jvm::jvm_libraries::JvmLibraries;
        use krusty::jvm::names::file_class_name;
        use krusty::lexer::lex;
        use krusty::parser::parse;
        use krusty::plugins::{serialization::SerializationPlugin, PluginContext, PluginHost};
        use krusty::resolve::{check_file, collect_signatures_with_cp};

        let cp = Rc::new(Classpath::new(vec![
            stdlib.clone(),
            core.clone(),
            json.clone(),
            jimage.clone(),
        ]));
        let src = "@Serializable class Foo(val a: Int, val b: String)";
        let mut d = DiagSink::new();
        let toks = lex(src, &mut d);
        let files = vec![parse(src, &toks, &mut d)];
        let platform = Box::new(JvmLibraries::new(cp.clone()));
        let syms = collect_signatures_with_cp(&files, platform, &mut d);
        let info = check_file(&files[0], &syms, &mut d);
        assert!(!d.has_errors(), "krusty front-end could not handle Foo");
        let mut ir = lower_file(&files[0], &info, &syms).expect("lower Foo");
        let ctx = PluginContext::from_source(&files[0], &ir);
        let mut host = PluginHost::new();
        host.register(Box::new(SerializationPlugin::default()));
        host.run(&mut ir, &ctx);
        let facade = file_class_name("Foo", files[0].package.as_deref());
        let classes = krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp, None)
            .expect("krusty emits Foo + Foo$serializer");

        let out = std::env::temp_dir().join(format!("krusty_serrt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        for (internal, bytes) in &classes {
            let p = out.join(format!("{internal}.class"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
        out
    };

    // 2. Real kotlinc compiles the box() driver (Json round-trip) against krusty's classes + runtime.
    let driver = classes_dir.join("Box.kt");
    std::fs::write(
        &driver,
        r#"import kotlinx.serialization.json.Json
import kotlinx.serialization.KSerializer
@Suppress("UNCHECKED_CAST")
fun box(): String {
    val s = Foo.serializer() as KSerializer<Foo>
    val j = Json.encodeToString(s, Foo(1, "x"))
    if (j != "{\"a\":1,\"b\":\"x\"}") return "ENC FAIL: $j"
    val back = Json.decodeFromString(s, j)
    if (back.a != 1 || back.b != "x") return "DEC FAIL: ${back.a},${back.b}"
    // A non-default payload, to prove decode actually reads the values (not defaults).
    val back2 = Json.decodeFromString(s, "{\"a\":42,\"b\":\"hi\"}")
    return if (back2.a == 42 && back2.b == "hi") "OK" else "DEC2 FAIL: ${back2.a},${back2.b}"
}
fun main() { println(box()) }
"#,
    )
    .unwrap();
    let java = jdk.join("bin/java");
    let driver_cp = format!(
        "{}:{}:{}:{}",
        classes_dir.display(),
        core.display(),
        json.display(),
        stdlib.display()
    );
    let compile = Command::new(&java)
        .arg("-cp")
        .arg(&cc)
        .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
        .args(["-cp", &driver_cp, "-no-stdlib", "-no-reflect", "-d"])
        .arg(&classes_dir)
        .arg(&driver)
        .output()
        .expect("run kotlinc");
    assert!(
        compile.status.success(),
        "kotlinc could not compile the driver against krusty's Foo:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // 3. Run box() on the JVM with the runtime; assert the round-trip is correct.
    let run = Command::new(&java)
        .arg("-cp")
        .arg(&driver_cp)
        .arg("BoxKt")
        .output()
        .expect("run box");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stdout.contains("OK"),
        "serialization encode round-trip failed.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("serialization encode round-trip OK — krusty's $serializer produced correct JSON");
}
