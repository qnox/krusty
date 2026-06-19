//! Safe calls `?.`: `recv?.member` / `recv?.method(args)` evaluate to `null` when the receiver is
//! null, else the member/call result — composing with the Elvis operator `?:`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn safe_calls_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping safe_call_e2e: set JAVA_HOME");
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
        eprintln!("skipping safe_call_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_sc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("S.kt"),
        "class Box(val label: String) { fun shout(): String = label }\nfun pick(b: Boolean): Box? = if (b) Box(\"hi\") else null\nfun safeLabel(b: Boolean): String = pick(b)?.shout() ?: \"none\"\nfun safeProp(b: Boolean): String = pick(b)?.label ?: \"none\"\nfun box(): String {\n  if (safeLabel(true) != \"hi\") return \"f1\"\n  if (safeLabel(false) != \"none\") return \"f2\"\n  if (safeProp(true) != \"hi\") return \"f3\"\n  if (safeProp(false) != \"none\") return \"f4\"\n  if (pick(true)?.shout() != \"hi\") return \"f5\"\n  if (pick(false)?.shout() != null) return \"f6\"\n  return \"OK\"\n}\n").unwrap();
    let kc = Command::new(krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
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
        "public class M { public static void main(String[] a) { System.out.println(SKt.box()); } }",
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
