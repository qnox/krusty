//! `iterable.forEach { … }` is the stdlib `inline fun` (`for (e in this) action(e)`), so krusty
//! inlines it to a for-each loop. That makes a *mutable capture* in the lambda work (`s += it`) —
//! which a non-inlined closure could not — exactly as kotlinc's inlining does. Run on a real JVM.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

const SRC: &str = r#"
fun box(): String {
    // mutable capture through forEach (inlined → no closure)
    var s = 0
    listOf(1, 2, 3, 4).forEach { s += it }
    if (s != 10) return "f1: $s"
    // a Set, with `it`
    var p = 1
    setOf(2, 3, 5).forEach { p *= it }
    if (p != 30) return "f2: $p"
    // non-mutating forEach still works
    val sb = StringBuilder()
    listOf("a", "b", "c").forEach { sb.append(it) }
    if (sb.toString() != "abc") return "f3: $sb"
    // forEachIndexed (inlined with an index counter) + mutable capture
    var w = 0
    listOf(10, 20, 30).forEachIndexed { i, x -> w += (i + 1) * x }
    if (w != 140) return "f4: $w"
    return "OK"
}
"#;

#[test]
fn foreach_inlines() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping foreach_inline_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping foreach_inline_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let jdk_modules = format!("{java_home}/lib/modules");
    let compile_cp = if std::path::Path::new(&jdk_modules).exists() {
        format!("{stdlib}:{jdk_modules}")
    } else {
        stdlib.clone()
    };
    let dir = std::env::temp_dir().join(format!("krusty_foreach_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("F.kt");
    fs::write(&src_path, SRC).unwrap();
    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin).args(["-cp", &compile_cp, "-d", dir.to_str().unwrap()]).arg(&src_path).output().unwrap();
    assert!(out.status.success(), "krusty: {}", String::from_utf8_lossy(&out.stderr));
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(FKt.box()); } }").unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let run = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}
