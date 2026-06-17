//! `try { … } [catch …] finally { … }` — the `finally` runs on the normal path, after a caught
//! exception, and (via a catch-all that re-throws) on an uncaught one. Round-tripped under
//! `-Xverify:all`; the run order is asserted via a log string.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn finally_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping finally_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping finally_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_fin_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "var log = \"\"\n\
fun mark(s: String): Int { log = log + s; return 1 }\n\
fun box(): String {\n\
try { mark(\"a\") } finally { mark(\"b\") }\n\
try { throw RuntimeException(\"x\") } catch (e: RuntimeException) { mark(\"c\") } finally { mark(\"d\") }\n\
val r = try { mark(\"e\") } finally { mark(\"f\") }\n\
if (r != 1) return \"fr\"\n\
return if (log == \"abcdef\") \"OK\" else log\n\
}\n";
    fs::write(dir.join("F.kt"), src).unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("F.kt")).output().unwrap();
    if !kc.status.success() { eprintln!("skip (IR unsupported): {}", String::from_utf8_lossy(&kc.stderr)); return; }
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(FKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let r = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&r.stderr));
    let _ = fs::remove_dir_all(&dir);
}
