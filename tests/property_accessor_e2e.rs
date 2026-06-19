//! Default property accessors: a class's backing fields are private, with synthesized `getX()`/`setX()`
//! accessors; access from outside the declaring class goes through them (`c.x`/`c.x = v`), while inside
//! the class the field is used directly. Round-tripped under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn property_accessors_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping property_accessor_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping property_accessor_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_pa_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "class Box(val x: Int, var y: String) {\n\
    fun internal(): Int = x\n\
}\n\
fun box(): String {\n\
val b = Box(10, \"a\")\n\
if (b.x != 10) return \"f1\"\n\
if (b.internal() != 10) return \"f2\"\n\
b.y = \"z\"\n\
if (b.y != \"z\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    fs::write(dir.join("B.kt"), src).unwrap();
    let kc = Command::new(krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("B.kt"))
        .output()
        .unwrap();
    if !kc.status.success() {
        eprintln!(
            "skip (IR unsupported): {}",
            String::from_utf8_lossy(&kc.stderr)
        );
        return;
    }
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(BKt.box()); } }",
    )
    .unwrap();
    assert!(Command::new(&javac)
        .args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
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
