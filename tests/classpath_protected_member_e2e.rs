//! Classpath `protected` visibility: a Kotlin subclass of a javac-compiled Java base can call the
//! base's `protected` instance method (unqualified, through the inherited receiver) — kotlinc accepts
//! this, and the JVM allows the invoke because the caller is a subclass. krusty must surface the
//! `protected` member during the supertype member walk (not drop it as non-public) and resolve an
//! inherited classpath-superclass member of a user class at all.

use std::fs;
use std::process::Command;

use super::common;

#[test]
fn subclass_calls_protected_classpath_member() {
    let Some(java_home) = common::java_home() else {
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = common::krusty_binary();
    let root = std::env::temp_dir().join(format!("krusty_prot_{}", std::process::id()));
    let cp = root.join("cp");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(cp.join("lib")).unwrap();

    // A Java base with a PROTECTED instance method — only reachable from a subclass.
    fs::write(
        cp.join("lib/Base.java"),
        "package lib;\npublic class Base {\n  protected int secret() { return 42; }\n}\n",
    )
    .unwrap();
    assert!(Command::new(&javac)
        .args(["-d", cp.to_str().unwrap()])
        .arg(cp.join("lib/Base.java"))
        .output()
        .unwrap()
        .status
        .success());

    // A Kotlin subclass calls the inherited protected member unqualified.
    fs::write(
        root.join("Use.kt"),
        "import lib.Base\nclass Sub : Base() {\n  fun reveal(): Int = secret()\n}\n\
         fun box(): String {\n  if (Sub().reveal() != 42) return \"f1\"\n  return \"OK\"\n}\n",
    )
    .unwrap();
    let kr = root.join("kr");
    let out = Command::new(&krusty)
        .args(["-cp", cp.to_str().unwrap(), "-d", kr.to_str().unwrap()])
        .arg(root.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "krusty failed to resolve the protected classpath member:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let main = "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    fs::write(kr.join("M.java"), main).unwrap();
    let stdlib = common::stdlib_jar()
        .map(|p| format!(":{}", p.display()))
        .unwrap_or_default();
    let kcp = format!(
        "{}:{}{}",
        kr.to_str().unwrap(),
        cp.to_str().unwrap(),
        stdlib
    );
    assert!(Command::new(&javac)
        .args(["-cp", &kcp, "-d", kr.to_str().unwrap()])
        .arg(kr.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(&java)
        .args(["-Xverify:all", "-cp", &kcp, "M"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = fs::remove_dir_all(&root);
}
