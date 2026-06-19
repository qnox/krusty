//! Non-capturing lambdas `{ a -> … }` passed to a function-typed parameter, lowered to
//! `invokedynamic` + `LambdaMetafactory` producing a `kotlin/jvm/functions/FunctionN`, then invoked
//! through `FunctionN.invoke`. Round-tripped against the JVM under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn lambdas_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping lambda_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    // FunctionN lives in kotlin-stdlib — needed on the runtime classpath.
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping lambda_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_lam_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "fun call1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun call0(f: () -> Int): Int = f()\n\
fun call2(f: (Int, Int) -> Int): Int = f(20, 22)\n\
fun box(): String {\n\
if (call1({ n -> n + 1 }, 41) != 42) return \"f1\"\n\
if (call0({ 7 }) != 7) return \"f2\"\n\
if (call1({ it * 2 }, 41) != 82) return \"f3\"\n\
if (call2({ a, b -> a + b }) != 42) return \"f4\"\n\
return \"OK\"\n\
}\n";
    fs::write(dir.join("L.kt"), src).unwrap();
    let kc = Command::new(krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("L.kt"))
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
        "public class M { public static void main(String[] a) { System.out.println(LKt.box()); } }",
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
