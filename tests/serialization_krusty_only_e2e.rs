//! PURE-KRUSTY serialization round-trip (encode): krusty alone compiles a `@Serializable` class, its
//! `$serializer` (the plugin), the `C.serializer()` accessor (signature phase + static-call lowering),
//! AND the `Json.encodeToString(C.serializer(), C(...))` call (classpath companion-instance call +
//! subtype-aware arg matching) — NO kotlinc anywhere. The JVM then runs `box()` against the published
//! kotlinx-serialization runtime and we assert the JSON. This is the whole serialization extension
//! exercised end-to-end through krusty's own front end + backend.
//!
//! Self-skips if the kotlinx-serialization runtime jars aren't locatable.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// Recursively find a `<prefix>*.jar` (no `-sources`) under `dir`.
fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Option<PathBuf>) {
    if out.is_some() || depth > 8 {
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

/// Locate a serialization runtime jar by prefix across the common cache roots (gradle/m2 + any
/// distribution-bundled gradle lib).
fn find(prefix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let mut roots = vec![
        PathBuf::from(&home).join(".gradle"),
        PathBuf::from(&home).join(".m2"),
    ];
    if let Ok(rd) = std::fs::read_dir("/opt/mise/installs/gradle") {
        roots.extend(rd.flatten().map(|e| e.path()));
    }
    let mut out = None;
    for r in &roots {
        walk(r, prefix, 0, &mut out);
        if out.is_some() {
            break;
        }
    }
    out
}

#[test]
fn serializable_class_encodes_to_json_entirely_in_krusty() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar located");
        return;
    };
    let (Some(core), Some(json)) = (
        find("kotlinx-serialization-core-jvm"),
        find("kotlinx-serialization-json-jvm"),
    ) else {
        eprintln!("skipping: kotlinx-serialization runtime jars not located");
        return;
    };
    let Some(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())
    else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let java = PathBuf::from(&java_home).join("bin/java");

    let cp_jars = vec![stdlib.clone(), core.clone(), json.clone()];

    // krusty compiles the WHOLE program (no kotlinc): the @Serializable class + $serializer + the
    // serializer() accessor + the Json.encodeToString(...) call.
    let src = r#"import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
@Serializable
class Foo(val a: Int, val b: String)
fun box(): String = Json.encodeToString(Foo.serializer(), Foo(1, "x"))
"#;
    let Some(classes) = common::compile_in_process(src, "SerRT", &cp_jars, None) else {
        panic!("krusty failed to compile the pure-krusty serialization program");
    };

    let out = std::env::temp_dir().join(format!("krusty_ser_only_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    for (internal, bytes) in &classes {
        let p = out.join(format!("{internal}.class"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    // Reflective launcher: invoke SerRTKt.box() and print the result.
    let launcher = out.join("Run.java");
    std::fs::write(
        &launcher,
        r#"public class Run { public static void main(String[] a) throws Exception {
        System.out.println(Class.forName("SerRTKt").getMethod("box").invoke(null)); } }"#,
    )
    .unwrap();
    let javac = PathBuf::from(&java_home).join("bin/javac");
    assert!(Command::new(&javac)
        .args(["-d", out.to_str().unwrap()])
        .arg(&launcher)
        .status()
        .unwrap()
        .success());

    let run = Command::new(&java)
        .arg("-cp")
        .arg(format!(
            "{}:{}:{}:{}",
            out.display(),
            stdlib.display(),
            core.display(),
            json.display()
        ))
        .arg("Run")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim() == "{\"a\":1,\"b\":\"x\"}",
        "krusty-only serialization encode wrong.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    eprintln!(
        "pure-krusty serialization encode round-trip OK: {}",
        stdout.trim()
    );
    let _ = std::fs::remove_dir_all(&out);
}
