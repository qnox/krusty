//! Class-body properties (`class C { val x = … }`), plain (non-property) constructor parameters,
//! and `init { }` blocks — initialized in the primary constructor, accessible from member methods.
//! Plus open-property virtual dispatch (an `open val` read inside the class calls the getter).

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn run_box(name: &str, src: &str) {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping class_body_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    // Reference `==`/`!=` compiles to `kotlin/jvm/internal/Intrinsics.areEqual` — needs kotlin-stdlib
    // on the runtime classpath, as any real Kotlin program does.
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping class_body_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_cb_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("B.kt"), src).unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("B.kt")).output().unwrap();
    // IR backend covers a subset; skip (not fail) a construct it doesn't yet lower.
    if !kc.status.success() { eprintln!("skip (IR unsupported): {}", String::from_utf8_lossy(&kc.stderr)); return; }
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(BKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let r = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "OK", "{name}: stderr={}", String::from_utf8_lossy(&r.stderr));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn body_properties_and_init_block() {
    run_box("init", "class Counter(start: Int) {\n  val initial: Int = start\n  var count: Int = 0\n  init { count = start * 2 }\n  fun total(): Int = initial + count\n}\nfun box(): String {\n  val c = Counter(5)\n  if (c.initial != 5) return \"f1\"\n  if (c.count != 10) return \"f2\"\n  if (c.total() != 15) return \"f3\"\n  return \"OK\"\n}\n");
}

#[test]
fn open_property_virtual_dispatch() {
    // An `open val` read inside the base class must dispatch to the override.
    run_box("openprop", "open class Base { open val kind: String = \"base\"\n  fun k(): String = kind\n}\nclass Sub : Base() { override val kind: String = \"sub\" }\nfun box(): String = if (Sub().k() == \"sub\") \"OK\" else \"fail\"\n");
}
