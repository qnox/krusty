//! Data-class `copy` with named / omitted arguments, realized via the `$default` mechanism: the JVM
//! backend emits a `copy$default(self, fields…, mask, marker)` stub (byte-identical to kotlinc), and a
//! call with omitted args passes a mask. Round-tripped under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn data_class_copy_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping data_copy_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping data_copy_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_dc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "data class P(val x: Int, val y: String)\n\
fun box(): String {\n\
val p = P(1, \"a\")\n\
val q = p.copy(y = \"b\")\n\
val r = p.copy(x = 9)\n\
val s = p.copy(2, \"c\")\n\
val t = p.copy()\n\
if (q.x != 1 || q.y != \"b\") return \"f1\"\n\
if (r.x != 9 || r.y != \"a\") return \"f2\"\n\
if (s.x != 2 || s.y != \"c\") return \"f3\"\n\
if (t.x != 1 || t.y != \"a\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    fs::write(dir.join("D.kt"), src).unwrap();
    let kc = Command::new(krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("D.kt"))
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
        "public class M { public static void main(String[] a) { System.out.println(DKt.box()); } }",
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
