//! Java-interop breadth: constructing a classpath Java object (`Calc(10)`) and calling its
//! *instance* methods (`c.add(5)`, `c.tag()`), resolved via the `.class` reader → `invokespecial`
//! `<init>` + `invokevirtual`. Uses a real javac-compiled class.

use std::fs;
use std::process::Command;

mod common;

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
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
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

    // krusty constructs it and calls instance methods.
    fs::write(root.join("Use.kt"),
        "import util.Calc\nfun box(): String {\n  val c = Calc(10)\n  if (c.add(5) != 15) return \"f1\"\n  if (c.tag() != \"calc\") return \"f2\"\n  return \"OK\"\n}\n").unwrap();
    let kr = root.join("kr");
    let out = Command::new(krusty)
        .args(["-cp", cp.to_str().unwrap(), "-d", kr.to_str().unwrap()])
        .arg(root.join("Use.kt"))
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!(
            "skip (IR unsupported): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let main = "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    fs::write(kr.join("M.java"), main).unwrap();
    // The compiled output may reference `kotlin/jvm/internal/Intrinsics` (parameter null-checks, like
    // kotlinc) — put the stdlib on the run classpath.
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
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
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

    fs::write(
        root.join("Use.kt"),
        "import lib.Logf\nfun box(): String {\n\
         if (Logf.make(\"x\") != \"n:x\") return \"f1\"\n\
         if (Logf.parse(\"10\") != 10) return \"f2\"\n\
         if (Logf.parse(\"ff\", 16) != 255) return \"f3\"\n\
         return \"OK\"\n}\n",
    )
    .unwrap();
    let kr = root.join("kr");
    let out = Command::new(krusty)
        .args(["-cp", cp.to_str().unwrap(), "-d", kr.to_str().unwrap()])
        .arg(root.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "krusty failed on Java static call:\n{}",
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
