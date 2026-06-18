//! A primitive lambda parameter is unboxed correctly even when the generic `FunctionN` signature
//! boxes it: `mapIndexed { i, x -> … }`'s index `i` is `Int`, not `java/lang/Integer`. Run on a real
//! JVM under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

const SRC: &str = r#"
fun box(): String {
    // The index `i` must be `Int` (the generic Function2 signature boxes it to Integer).
    val r = listOf(10, 20, 30).mapIndexed { i, x -> i * x + 1 }
    if (r != listOf(1, 21, 61)) return "f1: $r"
    val r2 = listOf("a", "bb", "ccc").mapIndexed { i, s -> i + s.length }
    if (r2 != listOf(1, 3, 5)) return "f2: $r2"
    return "OK"
}
"#;

#[test]
fn map_indexed_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping mapindexed_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping mapindexed_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let dir = std::env::temp_dir().join(format!("krusty_mapidx_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("R.kt");
    fs::write(&src_path, SRC).unwrap();
    // krusty resolves stdlib generic signatures + JDK types from the classpath: the stdlib jar plus
    // the running JDK's `lib/modules` jimage (like the conformance harness).
    let jdk_modules = format!("{java_home}/lib/modules");
    let compile_cp = if std::path::Path::new(&jdk_modules).exists() {
        format!("{stdlib}:{jdk_modules}")
    } else {
        stdlib.clone()
    };
    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin).args(["-cp", &compile_cp, "-d", dir.to_str().unwrap()]).arg(&src_path).output().unwrap();
    assert!(out.status.success(), "krusty: {}", String::from_utf8_lossy(&out.stderr));
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(RKt.box()); } }").unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let run = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}
