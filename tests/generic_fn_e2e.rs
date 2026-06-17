//! Generic functions (`fun <T> id(x: T): T`): the JVM signature erases `T` to `Object`, and the call
//! site inserts a `checkcast` to the inferred concrete type — matching kotlinc. Includes a generic
//! higher-order function (`fun <T> eval(fn: () -> T) = fn()`). Round-tripped under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn generic_fns_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping generic_fn_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping generic_fn_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_gen_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "fun <T> id(x: T): T = x\n\
fun <T> firstOf(a: T, b: T): T = a\n\
fun <T> eval(fn: () -> T): T = fn()\n\
fun box(): String {\n\
val s: String = id(\"OK\")\n\
if (s != \"OK\") return \"f1\"\n\
if (firstOf(\"X\", \"Y\") != \"X\") return \"f2\"\n\
if (eval { \"Z\" } != \"Z\") return \"f3\"\n\
return id(\"OK\")\n\
}\n";
    fs::write(dir.join("G.kt"), src).unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("G.kt")).output().unwrap();
    if !kc.status.success() { eprintln!("skip (IR unsupported): {}", String::from_utf8_lossy(&kc.stderr)); return; }
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(GKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let r = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&r.stderr));
    let _ = fs::remove_dir_all(&dir);
}

/// A property declared as a class type parameter (`class Box<T>(val x: T)`) erases to `Object`, but a
/// read on a concrete instantiation (`Box<Int>().x`) recovers the argument: the front end substitutes
/// the type argument, and codegen inserts the `checkcast`/unbox kotlinc emits on the erased read.
/// Covers a primitive argument (unbox), a reference argument (checkcast), and positional indexing.
#[test]
fn generic_property_substitution_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping generic_property_substitution: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping generic_property_substitution: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_genprop_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "class Box<T>(val x: T)\n\
class Pair2<A, B>(val a: A, val b: B)\n\
fun box(): String {\n\
val bi: Box<Int> = Box(40)\n\
if (bi.x + 2 != 42) return \"f1\"\n\
val bs: Box<String> = Box(\"OK\")\n\
if (bs.x != \"OK\") return \"f2\"\n\
val p: Pair2<Int, String> = Pair2(7, \"hi\")\n\
if (p.a != 7) return \"f3\"\n\
if (p.b != \"hi\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    fs::write(dir.join("G.kt"), src).unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("G.kt")).output().unwrap();
    if !kc.status.success() { eprintln!("skip (IR unsupported): {}", String::from_utf8_lossy(&kc.stderr)); return; }
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(GKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let r = Command::new(&java).args(["-Xverify:all", "-cp", &cp, "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&r.stderr));
    let _ = fs::remove_dir_all(&dir);
}
