//! Range expressions as first-class *values*: `a..b` (→ `IntRange`/`LongRange`), `a..<b`
//! (→ `RangesKt.until`). Members (`first`/`last`), `for`-iteration over a stored range value, and
//! the syntactic `for (i in a..b)` form all run on a real JVM under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

const SRC: &str = r#"
fun box(): String {
    val a = 0
    val b = 3
    val r = a..b
    if (r.first != 0) return "f1"
    if (r.last != 3) return "f2"
    var s = 0
    for (x in r) s += x
    if (s != 6) return "f3"
    val u = 1..<4
    if (u.last != 3) return "f4"
    val lr = 10L..12L
    if (lr.last != 12L) return "f5a"
    var lo = 0L
    for (y in lr) lo += y
    if (lo != 33L) return "f5"
    var t = 0
    for (z in 5..7) t += z
    if (t != 18) return "f6"
    // a Char counted for-range
    var cs = 0
    for (c in 'a'..'e') cs += c.code
    if (cs != 'a'.code + 'b'.code + 'c'.code + 'd'.code + 'e'.code) return "f7"
    // a Long counted for-range
    var lt = 0L
    for (y in 1L..4L) lt += y
    if (lt != 10L) return "f8"
    return "OK"
}
"#;

#[test]
fn range_values_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping range_value_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    // `..` constructs `kotlin.ranges.IntRange`, so kotlin-stdlib must be on the classpath.
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping range_value_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let dir = std::env::temp_dir().join(format!("krusty_range_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("R.kt");
    fs::write(&src_path, SRC).unwrap();
    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(&src_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "krusty: {}", String::from_utf8_lossy(&out.stderr));
    let main = "public class M { public static void main(String[] a) { System.out.println(RKt.box()); } }";
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac)
        .args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let run = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}
