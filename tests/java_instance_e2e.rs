//! Java-interop breadth: constructing a classpath Java object (`Calc(10)`) and calling its
//! *instance* methods (`c.add(5)`, `c.tag()`), resolved via the `.class` reader → `invokespecial`
//! `<init>` + `invokevirtual`. Uses a real javac-compiled class.
//!
//! Compiles Kotlin IN-PROCESS (`compile_to_dir`, warm classpath cache) and runs the Java driver
//! through the persistent `javac_run` server — never spawning the krusty CLI or a cold `java` per
//! case (see `tests/common`), so this stays fast even under coverage instrumentation.

use std::fs;
use std::process::Command;

use super::common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn constructs_and_calls_java_instance_methods() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping java_instance_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (Some(jdk), Some(stdlib)) = (common::jdk_modules(), common::stdlib_jar()) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("krusty_ji_{}", std::process::id()));
    let cp = root.join("cp");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(cp.join("util")).unwrap();

    // A plain Java class with a constructor + instance methods.
    fs::write(cp.join("util/Calc.java"),
        "package util;\npublic class Calc {\n  private final int base;\n  public Calc(int base) { this.base = base; }\n  public int add(int n) { return base + n; }\n  public String tag() { return \"calc\"; }\n}\n").unwrap();
    assert!(Command::new(&javac)
        .args(["-d", cp.to_str().unwrap()])
        .arg(cp.join("util/Calc.java"))
        .output()
        .unwrap()
        .status
        .success());

    // krusty constructs it and calls instance methods — compiled in-process against the cp dir.
    let use_src = "import util.Calc\nfun box(): String {\n  val c = Calc(10)\n  if (c.add(5) != 15) return \"f1\"\n  if (c.tag() != \"calc\") return \"f2\"\n  return \"OK\"\n}\n";
    let kr = root.join("kr");
    if common::compile_to_dir(use_src, "Use", std::slice::from_ref(&cp), Some(&jdk), &kr).is_none()
    {
        eprintln!("skip (IR unsupported)");
        return;
    }

    let main = "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    let m_path = kr.join("M.java");
    fs::write(&m_path, main).unwrap();
    // The compiled output may reference `kotlin/jvm/internal/Intrinsics` (parameter null-checks, like
    // kotlinc) — put the stdlib on the run classpath.
    let kcp = format!(
        "{}:{}:{}",
        kr.to_str().unwrap(),
        cp.to_str().unwrap(),
        stdlib.display()
    );
    let out = common::javac_run(m_path.to_str().unwrap(), &kcp, kr.to_str().unwrap(), "M");
    assert_eq!(out.as_deref().map(str::trim), Some("OK"), "run={out:?}");
    let _ = fs::remove_dir_all(&root);
}

/// Java (non-Kotlin) STATIC method resolution, including overload selection by argument type:
/// `Logf.make(String)` vs `Logf.make(Class)`, and `Logf.parse(String)` vs `Logf.parse(String, int)`.
/// krusty resolves the class-name receiver's static (from the `.class` reader → the type's static list),
/// picks the arity/type-appropriate overload, and emits `invokestatic`.
#[test]
fn calls_java_static_overloaded_methods() {
    let Some(java_home) = common::java_home() else {
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (Some(jdk), Some(stdlib)) = (common::jdk_modules(), common::stdlib_jar()) else {
        return;
    };
    let root = std::env::temp_dir().join(format!("krusty_js_{}", std::process::id()));
    let cp = root.join("cp");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(cp.join("lib")).unwrap();

    fs::write(
        cp.join("lib/Logf.java"),
        "package lib;\npublic class Logf {\n\
         public static String make(String name) { return \"n:\" + name; }\n\
         public static String make(Class<?> c) { return \"c:\" + c.getSimpleName(); }\n\
         public static int parse(String s) { return Integer.parseInt(s); }\n\
         public static int parse(String s, int radix) { return Integer.parseInt(s, radix); }\n\
         }\n",
    )
    .unwrap();
    assert!(Command::new(&javac)
        .args(["-d", cp.to_str().unwrap()])
        .arg(cp.join("lib/Logf.java"))
        .output()
        .unwrap()
        .status
        .success());

    let use_src = "import lib.Logf\nfun box(): String {\n\
         if (Logf.make(\"x\") != \"n:x\") return \"f1\"\n\
         if (Logf.parse(\"10\") != 10) return \"f2\"\n\
         if (Logf.parse(\"ff\", 16) != 255) return \"f3\"\n\
         return \"OK\"\n}\n";
    let kr = root.join("kr");
    assert!(
        common::compile_to_dir(use_src, "Use", std::slice::from_ref(&cp), Some(&jdk), &kr)
            .is_some(),
        "krusty failed on Java static call"
    );

    let main = "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    let m_path = kr.join("M.java");
    fs::write(&m_path, main).unwrap();
    let kcp = format!(
        "{}:{}:{}",
        kr.to_str().unwrap(),
        cp.to_str().unwrap(),
        stdlib.display()
    );
    let out = common::javac_run(m_path.to_str().unwrap(), &kcp, kr.to_str().unwrap(), "M");
    assert_eq!(out.as_deref().map(str::trim), Some("OK"), "run={out:?}");
    let _ = fs::remove_dir_all(&root);
}
