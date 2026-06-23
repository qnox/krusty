//! Stepped integer progressions in `for`-loops: single `step`, chained `step … step …`, `downTo`/
//! `until`/`reversed` combined with `step`, and a stored progression re-stepped. The progression's
//! `last` element is recomputed by the stdlib (`getProgressionLastElement`) for each `step`, so the
//! iterated values match kotlinc exactly. Round-tripped on the JVM under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn stepped_progressions_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping range_step_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping range_step_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_rstep_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // Each case's expected sum is computed from the exact progression the stdlib produces.
    let src = "fun box(): String {\n\
var a = 0; for (i in 0..6 step 2) a += i\n\
if (a != 12) return \"a\"\n\
var b = 0; for (i in 0..6 step 2 step 3) b += i\n\
if (b != 9) return \"b\"\n\
var c = 0; for (i in 0 until 6 step 2 step 3) c += i\n\
if (c != 3) return \"c\"\n\
var d = 0; for (i in 6 downTo 0 step 2) d += i\n\
if (d != 12) return \"d\"\n\
var e = 0; for (i in 6 downTo 0 step 2 step 3) e += i\n\
if (e != 9) return \"e\"\n\
val p = 0..6\n\
var f = 0; for (i in p step 3) f += i\n\
if (f != 9) return \"f\"\n\
var g = 0L; for (i in 0L..6L step 2L step 3L) g += i\n\
if (g != 9L) return \"g\"\n\
var h = \"\"; for (c in 'a'..'g' step 2) h += c\n\
if (h != \"aceg\") return \"h\"\n\
var k = \"\"; for (c in 'g' downTo 'a' step 2) k += c\n\
if (k != \"geca\") return \"k\"\n\
var m = 0u; for (i in 0u..6u step 2) m = m + i\n\
if (m != 12u) return \"m\"\n\
var n = 0u; for (i in 0u..6u step 2 step 3) n = n + i\n\
if (n != 9u) return \"n\"\n\
return \"OK\"\n\
}\n";
    fs::write(dir.join("R.kt"), src).unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("R.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed: {}",
        String::from_utf8_lossy(&kc.stderr)
    );
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(RKt.box()); } }",
    )
    .unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(&javac)
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let r = Command::new(&java)
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&r.stdout).trim(),
        "OK",
        "stderr={}",
        String::from_utf8_lossy(&r.stderr)
    );
    let _ = fs::remove_dir_all(&dir);
}
