//! `suspend` / coroutines support, built in vertical slices, each proven against the real kotlinc
//! ABI (see docs/SPEC.md). Slice 1: the suspend *calling convention* — a suspend function lowers to
//! a continuation-passing-style (CPS) signature: an extra `kotlin.coroutines.Continuation` parameter
//! is appended and the return type is erased to `java.lang.Object` (the resume value, boxed). A
//! "leaf" suspend function (no suspension point in its body) needs no state machine — kotlinc emits
//! exactly this shape.

use std::fs;
use std::process::Command;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn java_home() -> Option<String> {
    env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME"))
}

/// Compile `src` with the krusty binary into a fresh dir; return (dir, java_home) or `None` if javap
/// is unavailable (test then skips).
fn krusty_compile(name: &str, src: &str) -> Option<(std::path::PathBuf, String)> {
    let jh = java_home()?;
    if !std::path::Path::new(&format!("{jh}/bin/javap")).exists() {
        return None;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("S.kt"), src).unwrap();
    let out = Command::new(krusty)
        .args(["-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name}: krusty failed to compile:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some((dir, jh))
}

fn javap(jh: &str, class_file: &std::path::Path) -> String {
    let out = Command::new(format!("{jh}/bin/javap"))
        .args(["-c", "-p"])
        .arg(class_file)
        .output()
        .unwrap();
    assert!(out.status.success(), "javap failed on {class_file:?}");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Locate a real `kotlin-stdlib.jar` (the coroutine intrinsics — `Continuation`, `ContinuationImpl`,
/// `IntrinsicsKt`, `ResultKt` — live there) for the compile + run classpath. Mirrors how the box
/// harness finds it: a vendored `.kotlinc/<v>/.../lib/kotlin-stdlib.jar`, else `KRUSTY_KOTLINC`'s dist.
fn stdlib_jar() -> Option<String> {
    // Walk up from CWD looking for `.kotlinc/*/kotlinc/lib/kotlin-stdlib.jar`.
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if let Ok(versions) = fs::read_dir(dir.join(".kotlinc")) {
            for v in versions.flatten() {
                let jar = v.path().join("kotlinc/lib/kotlin-stdlib.jar");
                if jar.exists() {
                    return Some(jar.to_string_lossy().into_owned());
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[test]
fn krusty_compiled_suspend_dep_is_consumable() {
    // The suspend round-trip: krusty compiles a `suspend fun` to a classpath dir (emitting `@Metadata`
    // with IS_SUSPEND + the logical signature), then krusty compiles a CALLER against that dir. Without
    // the metadata writer the callee's physical `Object helper(Continuation)` is unresolvable as
    // `helper()`. Drives UseKt.caller(k) → 43.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_rt_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    fs::create_dir_all(&lib).unwrap();
    // 1) krusty compiles the suspend lib (emits @Metadata).
    fs::write(dir.join("Lib.kt"), "suspend fun helper(): Int = 42\n").unwrap();
    let kl = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", lib.to_str().unwrap()])
        .arg(dir.join("Lib.kt"))
        .output()
        .unwrap();
    assert!(
        kl.status.success(),
        "krusty failed on lib:\n{}",
        String::from_utf8_lossy(&kl.stderr)
    );
    // 2) krusty compiles the caller against the krusty-compiled lib.
    fs::write(
        dir.join("Use.kt"),
        "suspend fun caller(): Int {\n    val a = helper()\n    return a + 1\n}\n",
    )
    .unwrap();
    let cp_compile = format!("{}:{}", lib.to_str().unwrap(), stdlib);
    let ku = Command::new(krusty)
        .args(["-cp", &cp_compile, "-d", dir.to_str().unwrap()])
        .arg(dir.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        ku.status.success(),
        "krusty failed resolving krusty-compiled suspend dep:\n{}",
        String::from_utf8_lossy(&ku.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Object r = UseKt.caller(k);\n\
    System.out.println(r.equals(Integer.valueOf(43)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!(
        "{}:{}:{}",
        dir.to_str().unwrap(),
        lib.to_str().unwrap(),
        stdlib
    );
    let jc = Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(
        jc.status.success(),
        "javac driver failed:\n{}",
        String::from_utf8_lossy(&jc.stderr)
    );
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "suspend round-trip: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_control_flow_with_capture_runs() {
    // A `suspend` lambda whose VALUE is a conditional suspension over a captured variable
    // (`{ if (c) foo() else 7 }`). Only the `c == true` branch suspends. make(true)→42, make(false)→7.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lamcf_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun foo(): Int = 42\nfun make(c: Boolean): suspend () -> Int = {\n    if (c) foo() else 7\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  static Continuation<Object> k() {\n\
    return new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
  }\n\
  public static void main(String[] a) {\n\
    Object r1 = ((Function1) SKt.make(true)).invoke(k());\n\
    Object r2 = ((Function1) SKt.make(false)).invoke(k());\n\
    boolean ok = r1.equals(Integer.valueOf(42)) && r2.equals(Integer.valueOf(7));\n\
    System.out.println(ok ? \"OK\" : (\"r1=\" + r1 + \" r2=\" + r2));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "control-flow+capture lambda: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_param_with_suspension_runs() {
    // A `suspend` lambda with its OWN parameter that ALSO suspends (`{ val a = foo(); it + a }`). The
    // parameter `it` is a field (set by `create`) reloaded into a local each invokeSuspend entry, like
    // a capture. make().invoke(10, k) → 10 + 42 = 52.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lampsusp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun foo(): Int = 42\nfun make(): suspend (Int) -> Int = {\n    val a = foo()\n    it + a\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function2;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Function2 f = SKt.make();\n\
    Object r = f.invoke(Integer.valueOf(10), k);\n\
    System.out.println(r.equals(Integer.valueOf(52)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "param+suspension lambda: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_captures_with_suspension_runs() {
    // A `suspend` lambda that BOTH captures an enclosing variable AND suspends (`{ n + foo() }`). The
    // capture `n` is a field reloaded into a local at each invokeSuspend entry; the suspension threads
    // `this`. make(10).invoke(k) → 10 + 42 = 52.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lamcapsusp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun foo(): Int = 42\nfun make(n: Int): suspend () -> Int = {\n    val a = foo()\n    n + a\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Function1 f = SKt.make(10);\n\
    Object r = f.invoke(k);\n\
    System.out.println(r.equals(Integer.valueOf(52)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "capture+suspension lambda: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_two_suspensions_async_resume() {
    // The ASYNC path of the general lambda-mode machine: `{ val a = suspendOnce(); val b = plain();
    // a + b }`. The first callee PARKS (returns COROUTINE_SUSPENDED); `a` must be SPILLED across the
    // second suspension. invoke suspends, resumeSaved(42) re-enters → 42 + 100 = 142.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let (Some(stdlib), Some(kotlinc)) = (stdlib_jar(), kotlinc_bin()) else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lam2as_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("Lib.kt"),
        "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var saved: Continuation<Int>? = null\n\
suspend fun suspendOnce(): Int = suspendCoroutineUninterceptedOrReturn { c ->\n\
    saved = c\n\
    COROUTINE_SUSPENDED\n\
}\n\
suspend fun plain(): Int = 100\n\
fun resumeSaved(v: Int) { saved!!.resumeWith(Result.success(v)) }\n",
    )
    .unwrap();
    let libjar = dir.join("lib.jar");
    let kc = Command::new(&kotlinc)
        .env("JAVA_HOME", &jh)
        .args(["-d", libjar.to_str().unwrap()])
        .arg(dir.join("Lib.kt"))
        .output()
        .unwrap();
    if !kc.status.success() {
        eprintln!(
            "skipping: kotlinc failed:\n{}",
            String::from_utf8_lossy(&kc.stderr)
        );
        return;
    }
    fs::write(
        dir.join("Use.kt"),
        "fun make(): suspend () -> Int = {\n    val a = suspendOnce()\n    val b = plain()\n    a + b\n}\n",
    )
    .unwrap();
    let cp_compile = format!("{}:{}", libjar.to_str().unwrap(), stdlib);
    let ku = Command::new(krusty)
        .args(["-cp", &cp_compile, "-d", dir.to_str().unwrap()])
        .arg(dir.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        ku.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&ku.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.coroutines.intrinsics.IntrinsicsKt;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    final Object[] box = new Object[1];\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { box[0] = o; }\n\
    };\n\
    Object r = UseKt.make().invoke(k);\n\
    boolean suspended = (r == IntrinsicsKt.getCOROUTINE_SUSPENDED());\n\
    LibKt.resumeSaved(42);\n\
    System.out.println(suspended && Integer.valueOf(142).equals(box[0]) ? \"OK\" : (\"s=\" + suspended + \" box=\" + box[0]));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!(
        "{}:{}:{}",
        dir.to_str().unwrap(),
        libjar.to_str().unwrap(),
        stdlib
    );
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "async two-suspension lambda: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_two_suspensions_runs() {
    // A `suspend` lambda with TWO suspension points (`{ val a = foo(); val b = bar(); a + b }`). Its
    // invokeSuspend needs a multi-state machine (the lambda instance as the continuation) — the general
    // lambda-mode flattener. Both callees complete synchronously → make().invoke(k) = 42 + 100 = 142.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lam2_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun foo(): Int = 42\nsuspend fun bar(): Int = 100\nfun make(): suspend () -> Int = {\n    val a = foo()\n    val b = bar()\n    a + b\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Function1 f = SKt.make();\n\
    Object r = f.invoke(k);\n\
    System.out.println(r.equals(Integer.valueOf(142)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "two-suspension lambda: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_non_tail_body_runs() {
    // A `suspend` lambda whose body BINDS a suspension result and then computes a tail expression
    // (`{ val a = foo(); a + 1 }`). The `invokeSuspend` state machine resumes into the binding, then
    // runs the tail. foo completes synchronously → make().invoke(k) yields boxed 43.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lamnontail_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun foo(): Int = 42\nfun make(): suspend () -> Int = {\n    val a = foo()\n    a + 1\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Function1 f = SKt.make();\n\
    Object r = f.invoke(k);\n\
    System.out.println(r.equals(Integer.valueOf(43)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "non-tail suspend lambda: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_fun_suspension_in_and_condition() {
    // A suspension on the RHS of `&&` in an `if` CONDITION (`if (c && check())`). The condition is
    // evaluated (and suspends) before the branch; only the `c == true` path calls `check()`. Drives:
    // bar(true) → check() true → 1; bar(false) → short-circuits (no suspension) → 2.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_andcond_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun check(): Boolean = true\n\
         suspend fun bar(c: Boolean): Int {\n    if (c && check()) return 1\n    return 2\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
public class M {\n\
  static Continuation<Object> k() {\n\
    return new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
  }\n\
  public static void main(String[] a) {\n\
    Object r1 = SKt.bar(true, k());\n\
    Object r2 = SKt.bar(false, k());\n\
    boolean ok = r1.equals(Integer.valueOf(1)) && r2.equals(Integer.valueOf(2));\n\
    System.out.println(ok ? \"OK\" : (\"r1=\" + r1 + \" r2=\" + r2));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "&& condition suspension: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_with_parameter_runs() {
    // A `suspend` lambda with its OWN parameter (`{ it + 1 }`, type `suspend (Int) -> Int`). The
    // parameter is a field set by `create(value, completion)`; `invoke(p, completion)` boxes p, calls
    // create, then invokeSuspend. The lambda implements Function2<Integer, Continuation, Object>.
    // Driven: make().invoke(10, k) -> 11.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lamparam_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "fun make(): suspend (Int) -> Int = { it + 1 }\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function2;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Function2 f = SKt.make();\n\
    Object r = f.invoke(Integer.valueOf(10), k);\n\
    System.out.println(r.equals(Integer.valueOf(11)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "suspend lambda param: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_internal_suspension_async_resume() {
    // The ASYNC path for an internal-suspension lambda: `{ suspendOnce() }` where suspendOnce (kotlinc)
    // parks the continuation. The lambda's invokeSuspend returns COROUTINE_SUSPENDED up; a later
    // resumeWith re-enters it (state 1) and delivers the value. make().invoke(k) suspends, then 42.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let (Some(stdlib), Some(kotlinc)) = (stdlib_jar(), kotlinc_bin()) else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_laminas_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("Lib.kt"),
        "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var saved: Continuation<Int>? = null\n\
suspend fun suspendOnce(): Int = suspendCoroutineUninterceptedOrReturn { c ->\n\
    saved = c\n\
    COROUTINE_SUSPENDED\n\
}\n\
fun resumeSaved(v: Int) { saved!!.resumeWith(Result.success(v)) }\n",
    )
    .unwrap();
    let libjar = dir.join("lib.jar");
    let kc = Command::new(&kotlinc)
        .env("JAVA_HOME", &jh)
        .args(["-d", libjar.to_str().unwrap()])
        .arg(dir.join("Lib.kt"))
        .output()
        .unwrap();
    if !kc.status.success() {
        eprintln!(
            "skipping: kotlinc failed:\n{}",
            String::from_utf8_lossy(&kc.stderr)
        );
        return;
    }
    fs::write(
        dir.join("Use.kt"),
        "fun make(): suspend () -> Int = { suspendOnce() }\n",
    )
    .unwrap();
    let cp_compile = format!("{}:{}", libjar.to_str().unwrap(), stdlib);
    let ku = Command::new(krusty)
        .args(["-cp", &cp_compile, "-d", dir.to_str().unwrap()])
        .arg(dir.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        ku.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&ku.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.coroutines.intrinsics.IntrinsicsKt;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    final Object[] box = new Object[1];\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { box[0] = o; }\n\
    };\n\
    Object r = UseKt.make().invoke(k);\n\
    boolean suspended = (r == IntrinsicsKt.getCOROUTINE_SUSPENDED());\n\
    LibKt.resumeSaved(42);\n\
    boolean delivered = Integer.valueOf(42).equals(box[0]);\n\
    System.out.println(suspended && delivered ? \"OK\" : (\"suspended=\" + suspended + \" box=\" + box[0]));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!(
        "{}:{}:{}",
        dir.to_str().unwrap(),
        libjar.to_str().unwrap(),
        stdlib
    );
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "async internal-suspension lambda: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_with_internal_suspension_runs() {
    // A `suspend` lambda whose body SUSPENDS (`{ foo() }`, foo a suspend fn). Its `invokeSuspend` is a
    // state machine with the lambda instance itself as the continuation. foo completes synchronously →
    // make().invoke(k) yields boxed 42.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_laminsusp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun foo(): Int = 42\nfun make(): suspend () -> Int = { foo() }\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Function1 f = SKt.make();\n\
    Object r = f.invoke(k);\n\
    System.out.println(r.equals(Integer.valueOf(42)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "suspend lambda internal suspension: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_lambda_captures_enclosing_variable() {
    // A `suspend` lambda capturing an enclosing parameter (`{ n + 1 }`). The captured value becomes a
    // field on the `SuspendLambda` subclass, set at construction and copied into the fresh instance
    // `invoke` builds. Driven: make(10).invoke(k) -> 10 + 1 = 11.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lamcap_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "fun make(n: Int): suspend () -> Int = { n + 1 }\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Function1 f = SKt.make(10);\n\
    Object r = f.invoke(k);\n\
    System.out.println(r.equals(Integer.valueOf(11)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "suspend lambda capture: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn leaf_suspend_lambda_creates_and_invokes() {
    // A leaf `suspend` lambda (`{ 42 }`, no captures, no internal suspension) compiles to a concrete
    // `SuspendLambda` subclass implementing `Function1<Continuation,Object>`, NOT krusty's
    // invokedynamic path. A Java driver gets the returned `Function1` and invokes it with a
    // continuation; the synchronously-completing body yields boxed 42.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_lam_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("S.kt"), "fun make(): suspend () -> Int = { 42 }\n").unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.jvm.functions.Function1;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Function1 f = SKt.make();\n\
    Object r = f.invoke(k);\n\
    System.out.println(r.equals(Integer.valueOf(42)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "leaf suspend lambda: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_function_type_lowers_to_function1_continuation() {
    // A `suspend () -> Int` parameter type must lower to kotlinc's representation
    // `Function1<? super Continuation<? super Integer>, ? extends Object>` — the suspend arity is the
    // logical arity PLUS one (the trailing continuation), with the body's value erased to Object. krusty
    // historically erased the `suspend` modifier and emitted `Function0` (a miscompile vs kotlinc).
    let Some((dir, jh)) = krusty_compile("susfty", "fun take(block: suspend () -> Int) {}\n")
    else {
        return;
    };
    let text = javap(&jh, &dir.join("SKt.class"));
    let _ = fs::remove_dir_all(&dir);
    assert!(
        text.contains("void take(kotlin.jvm.functions.Function1"),
        "suspend `() -> Int` param must lower to Function1<Continuation,…>, got:\n{text}"
    );
}

#[test]
fn suspend_fun_suspension_on_elvis_rhs() {
    // A suspension on the RHS of an elvis (`x ?: fallback()`) — a CONDITIONAL suspension (only the
    // null case suspends). Drives both: `bar(null)` takes the suspending branch → 7+1=8; `bar(5)`
    // takes the value branch (no suspension) → 5+1=6.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_elvis_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun fallback(): Int = 7\n\
         suspend fun bar(x: Int?): Int {\n    val a = x ?: fallback()\n    return a + 1\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
public class M {\n\
  static Continuation<Object> k() {\n\
    return new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
  }\n\
  public static void main(String[] a) {\n\
    Object r1 = SKt.bar(null, k());\n\
    Object r2 = SKt.bar(Integer.valueOf(5), k());\n\
    boolean ok = r1.equals(Integer.valueOf(8)) && r2.equals(Integer.valueOf(6));\n\
    System.out.println(ok ? \"OK\" : (\"r1=\" + r1 + \" r2=\" + r2));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "elvis suspension: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn leaf_suspend_fun_has_cps_signature() {
    // `suspend fun foo(): Int = 42` has no suspension point, so kotlinc emits no state machine — just
    // the CPS signature: `static Object foo(Continuation<? super Integer>)` returning the boxed value.
    let Some((dir, jh)) = krusty_compile("leaf", "suspend fun foo(): Int = 42\n") else {
        return;
    };
    let text = javap(&jh, &dir.join("SKt.class"));
    let _ = fs::remove_dir_all(&dir);
    assert!(
        text.contains("java.lang.Object foo(kotlin.coroutines.Continuation"),
        "leaf suspend fun must lower to CPS `Object foo(Continuation)`:\n{text}"
    );
    // No state-machine class is generated for a leaf function.
    assert!(
        !text.contains("SKt$foo$1"),
        "leaf suspend fun must NOT generate a continuation class:\n{text}"
    );
}

/// Compile `src` with krusty, then run a Java driver that calls the top-level suspend `fn`(Continuation)
/// with a trivial `Continuation` and asserts the (synchronously-completing) result equals `expect`.
/// The suspend callees complete synchronously (never COROUTINE_SUSPENDED), so the whole state machine
/// runs to completion under `-Xverify:all`. Skips if javac / kotlin-stdlib is unavailable.
fn run_suspend(name: &str, src: &str, call: &str, expect: i32) {
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib.jar found");
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("S.kt"), src).unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "{name}: krusty failed to compile:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = format!(
        "import kotlin.coroutines.Continuation;\n\
import kotlin.coroutines.CoroutineContext;\n\
import kotlin.coroutines.EmptyCoroutineContext;\n\
public class M {{\n\
  public static void main(String[] a) {{\n\
    Continuation<Object> k = new Continuation<Object>() {{\n\
      public CoroutineContext getContext() {{ return EmptyCoroutineContext.INSTANCE; }}\n\
      public void resumeWith(Object o) {{ }}\n\
    }};\n\
    Object r = SKt.{call}(k);\n\
    System.out.println(r.equals(Integer.valueOf({expect})) ? \"OK\" : (\"r=\" + r));\n\
  }}\n\
}}\n"
    );
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let jc = Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(
        jc.status.success(),
        "{name}: javac driver failed:\n{}",
        String::from_utf8_lossy(&jc.stderr)
    );
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "{name}: wrong result; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Like `run_suspend`, but compiles TWO source files in one krusty invocation. The callee lives in a
/// different file (a separate `IrFile`), so its suspend-ness is NOT in the caller file's
/// `suspend_funs` — the coroutine pass must learn it from the resolver (`@Metadata`/module symbols).
/// `call` is `Facade.method`, driven as `Facade.method(k)`.
fn run_suspend_2(name: &str, lib: &str, user: &str, facade: &str, method: &str, expect: i32) {
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Lib.kt"), lib).unwrap();
    fs::write(dir.join("Use.kt"), user).unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("Lib.kt"))
        .arg(dir.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "{name}: krusty failed to compile:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = format!(
        "import kotlin.coroutines.*;\n\
public class M {{\n\
  public static void main(String[] a) {{\n\
    Continuation<Object> k = new Continuation<Object>() {{\n\
      public CoroutineContext getContext() {{ return EmptyCoroutineContext.INSTANCE; }}\n\
      public void resumeWith(Object o) {{ }}\n\
    }};\n\
    Object r = {facade}.{method}(k);\n\
    System.out.println(r.equals(Integer.valueOf({expect})) ? \"OK\" : (\"r=\" + r));\n\
  }}\n\
}}\n"
    );
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let jc = Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(
        jc.status.success(),
        "{name}: javac driver failed:\n{}",
        String::from_utf8_lossy(&jc.stderr)
    );
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "{name}: wrong result; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_fun_calls_cross_file_suspend_fun() {
    // `caller` (Use.kt) suspends on `helper` (Lib.kt) — a different `IrFile`. The pass must recognize
    // the cross-file suspend call via the resolver (not the local `suspend_funs`). 42 + 1 = 43.
    run_suspend_2(
        "xfile",
        "suspend fun helper(): Int = 42\n",
        "suspend fun caller(): Int {\n    val a = helper()\n    return a + 1\n}\n",
        "UseKt",
        "caller",
        43,
    );
}

/// Locate the vendored real `kotlinc` launcher (same `.kotlinc/<v>/…` tree as `stdlib_jar`).
fn kotlinc_bin() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if let Ok(versions) = fs::read_dir(dir.join(".kotlinc")) {
            for v in versions.flatten() {
                let bin = v.path().join("kotlinc/bin/kotlinc");
                if bin.exists() {
                    return Some(bin.to_string_lossy().into_owned());
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[test]
fn suspend_fun_calls_classpath_suspend_fun() {
    // The callee is a REAL classpath dependency: `helper` is compiled by kotlinc into a jar (so its
    // `@Metadata` carries `IS_SUSPEND` + the logical signature, and the physical method is
    // `Object helper(Continuation)`). krusty then compiles the caller against `-cp lib.jar`. The
    // classpath parser must resolve `helper()` by its LOGICAL signature (no continuation arg, `Int`
    // return) and mark it suspend; the coroutine pass threads the continuation. 42 + 1 = 43.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let (Some(stdlib), Some(kotlinc)) = (stdlib_jar(), kotlinc_bin()) else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_cp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // 1) kotlinc builds the suspend lib into lib.jar.
    fs::write(dir.join("Lib.kt"), "suspend fun helper(): Int = 42\n").unwrap();
    let libjar = dir.join("lib.jar");
    let kc = Command::new(&kotlinc)
        .env("JAVA_HOME", &jh)
        .args(["-d", libjar.to_str().unwrap()])
        .arg(dir.join("Lib.kt"))
        .output()
        .unwrap();
    if !kc.status.success() {
        // kotlinc unavailable/incompatible in this env — skip rather than fail spuriously.
        eprintln!(
            "skipping: kotlinc failed:\n{}",
            String::from_utf8_lossy(&kc.stderr)
        );
        return;
    }
    // 2) krusty compiles the caller against the lib jar + stdlib.
    fs::write(
        dir.join("Use.kt"),
        "suspend fun caller(): Int {\n    val a = helper()\n    return a + 1\n}\n",
    )
    .unwrap();
    let cp_compile = format!("{}:{}", libjar.to_str().unwrap(), stdlib);
    let ku = Command::new(krusty)
        .args(["-cp", &cp_compile, "-d", dir.to_str().unwrap()])
        .arg(dir.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        ku.status.success(),
        "krusty failed to compile against classpath suspend dep:\n{}",
        String::from_utf8_lossy(&ku.stderr)
    );
    // 3) drive UseKt.caller(k) → 43.
    let driver = "import kotlin.coroutines.*;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Object r = UseKt.caller(k);\n\
    System.out.println(r.equals(Integer.valueOf(43)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!(
        "{}:{}:{}",
        dir.to_str().unwrap(),
        libjar.to_str().unwrap(),
        stdlib
    );
    let jc = Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(
        jc.status.success(),
        "javac driver failed:\n{}",
        String::from_utf8_lossy(&jc.stderr)
    );
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "classpath suspend call: wrong result; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_fun_with_suspension_point_runs_via_continuation() {
    // `bar` calls the suspend `foo` (one suspension point) → a state machine + continuation class.
    run_suspend(
        "sm1",
        "suspend fun foo(): Int = 42\n\
         suspend fun bar(): Int {\n    val a = foo()\n    return a + 1\n}\n",
        "bar",
        43,
    );
}

#[test]
fn suspend_fun_two_suspension_points_spills_live_local() {
    // `baz` has TWO suspension points; `a` (the first result) is live across the second call, so it
    // must be spilled to a continuation field and restored. Drives to 42 + 100 = 142.
    run_suspend(
        "sm2",
        "suspend fun foo(): Int = 42\n\
         suspend fun hundred(): Int = 100\n\
         suspend fun baz(): Int {\n    val a = foo()\n    val b = hundred()\n    return a + b\n}\n",
        "baz",
        142,
    );
}

#[test]
fn suspend_fun_suspension_inside_if_taken() {
    // The suspension `foo()` is inside the THEN branch of an `if` (`flag` is true). The state machine
    // must resume into the branch and converge at the merge. 42 + 1 = 43.
    run_suspend(
        "smif_t",
        "suspend fun foo(): Int = 42\n\
         val flag = true\n\
         suspend fun cond(): Int {\n    val a = if (flag) foo() else 7\n    return a + 1\n}\n",
        "cond",
        43,
    );
}

#[test]
fn suspend_fun_suspension_inside_if_not_taken() {
    // Same shape, `flag` false: the suspending branch is skipped, the else value (7) flows to the merge.
    run_suspend(
        "smif_f",
        "suspend fun foo(): Int = 42\n\
         val flag = false\n\
         suspend fun cond(): Int {\n    val a = if (flag) foo() else 7\n    return a + 1\n}\n",
        "cond",
        8,
    );
}

#[test]
fn state_machine_member_suspend_fun_runs() {
    // A member suspend fn that SUSPENDS (calls `foo`): its continuation `C$m$1` must capture the
    // receiver and, on resume, call `receiver.m(continuation)`. Driven: new C(100).m(k) -> 100+42=142.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_smmem_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun foo(): Int = 42\n\
         class C(val base: Int) {\n    suspend fun m(): Int {\n        val a = foo()\n        return base + a\n    }\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Object r = new C(100).m(k);\n\
    System.out.println(r.equals(Integer.valueOf(142)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn state_machine_member_suspend_fun_with_param_runs() {
    // A member suspend fn that SUSPENDS and has its OWN parameter `x`, live across the suspension:
    // the continuation `C$m$1` must capture the receiver AND spill `x` (restored on resume). Driven:
    // new C(100).m(5, k) -> 100 + 42 + 5 = 147.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_smmemp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "suspend fun foo(): Int = 42\n\
         class C(val base: Int) {\n    suspend fun m(x: Int): Int {\n        val a = foo()\n        return base + a + x\n    }\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Object r = new C(100).m(5, k);\n\
    System.out.println(r.equals(Integer.valueOf(147)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn toplevel_suspend_fun_with_param_survives_async_resume() {
    // ASYNC case for a TOP-LEVEL suspend fn with a live parameter `x` (the `receiver=None` capture
    // path). `caller(5)` suspends on `suspendOnce`; on resume, `x` must be restored from the captured
    // continuation field. 42 + 5 = 47.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let (Some(stdlib), Some(kotlinc)) = (stdlib_jar(), kotlinc_bin()) else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_tlp_async_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("Lib.kt"),
        "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var saved: Continuation<Int>? = null\n\
suspend fun suspendOnce(): Int = suspendCoroutineUninterceptedOrReturn { c ->\n\
    saved = c\n\
    COROUTINE_SUSPENDED\n\
}\n\
fun resumeSaved(v: Int) { saved!!.resumeWith(Result.success(v)) }\n",
    )
    .unwrap();
    let libjar = dir.join("lib.jar");
    let kc = Command::new(&kotlinc)
        .env("JAVA_HOME", &jh)
        .args(["-d", libjar.to_str().unwrap()])
        .arg(dir.join("Lib.kt"))
        .output()
        .unwrap();
    if !kc.status.success() {
        eprintln!(
            "skipping: kotlinc failed:\n{}",
            String::from_utf8_lossy(&kc.stderr)
        );
        return;
    }
    fs::write(
        dir.join("Use.kt"),
        "suspend fun caller(x: Int): Int {\n    val a = suspendOnce()\n    return a + x\n}\n",
    )
    .unwrap();
    let cp_compile = format!("{}:{}", libjar.to_str().unwrap(), stdlib);
    let ku = Command::new(krusty)
        .args(["-cp", &cp_compile, "-d", dir.to_str().unwrap()])
        .arg(dir.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        ku.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&ku.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.coroutines.intrinsics.IntrinsicsKt;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    final Object[] box = new Object[1];\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { box[0] = o; }\n\
    };\n\
    Object r = UseKt.caller(5, k);\n\
    boolean suspended = (r == IntrinsicsKt.getCOROUTINE_SUSPENDED());\n\
    LibKt.resumeSaved(42);\n\
    boolean delivered = Integer.valueOf(47).equals(box[0]);\n\
    System.out.println(suspended && delivered ? \"OK\" : (\"suspended=\" + suspended + \" box=\" + box[0]));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!(
        "{}:{}:{}",
        dir.to_str().unwrap(),
        libjar.to_str().unwrap(),
        stdlib
    );
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "async toplevel-param: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn member_suspend_fun_with_param_survives_async_resume() {
    // The ASYNC case for a member suspend fn with a live parameter: `x` must survive a real
    // suspension/resume. `suspendOnce` (kotlinc) parks the continuation; `m` propagates
    // COROUTINE_SUSPENDED, and on `resumeSaved(42)` re-enters — `x` (and the receiver `base`) must be
    // restored from the continuation's captured fields. new C(100).m(5): 100 + 42 + 5 = 147.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let (Some(stdlib), Some(kotlinc)) = (stdlib_jar(), kotlinc_bin()) else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_memp_async_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("Lib.kt"),
        "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var saved: Continuation<Int>? = null\n\
suspend fun suspendOnce(): Int = suspendCoroutineUninterceptedOrReturn { c ->\n\
    saved = c\n\
    COROUTINE_SUSPENDED\n\
}\n\
fun resumeSaved(v: Int) { saved!!.resumeWith(Result.success(v)) }\n",
    )
    .unwrap();
    let libjar = dir.join("lib.jar");
    let kc = Command::new(&kotlinc)
        .env("JAVA_HOME", &jh)
        .args(["-d", libjar.to_str().unwrap()])
        .arg(dir.join("Lib.kt"))
        .output()
        .unwrap();
    if !kc.status.success() {
        eprintln!(
            "skipping: kotlinc failed:\n{}",
            String::from_utf8_lossy(&kc.stderr)
        );
        return;
    }
    fs::write(
        dir.join("Use.kt"),
        "class C(val base: Int) {\n    suspend fun m(x: Int): Int {\n        val a = suspendOnce()\n        return base + a + x\n    }\n}\n",
    )
    .unwrap();
    let cp_compile = format!("{}:{}", libjar.to_str().unwrap(), stdlib);
    let ku = Command::new(krusty)
        .args(["-cp", &cp_compile, "-d", dir.to_str().unwrap()])
        .arg(dir.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        ku.status.success(),
        "krusty failed:\n{}",
        String::from_utf8_lossy(&ku.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.coroutines.intrinsics.IntrinsicsKt;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    final Object[] box = new Object[1];\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { box[0] = o; }\n\
    };\n\
    Object r = new C(100).m(5, k);\n\
    boolean suspended = (r == IntrinsicsKt.getCOROUTINE_SUSPENDED());\n\
    LibKt.resumeSaved(42);\n\
    boolean delivered = Integer.valueOf(147).equals(box[0]);\n\
    System.out.println(suspended && delivered ? \"OK\" : (\"suspended=\" + suspended + \" box=\" + box[0]));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!(
        "{}:{}:{}",
        dir.to_str().unwrap(),
        libjar.to_str().unwrap(),
        stdlib
    );
    assert!(Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap()
        .status
        .success());
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "async member-param: stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn leaf_member_suspend_fun_runs() {
    // A leaf `suspend` member function: it gets the CPS signature on the instance method (`Object
    // m(Continuation)`), no state machine. A Java driver creates the instance and calls it: 100+5=105.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_mem_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("S.kt"),
        "class C(val base: Int) {\n    suspend fun m(): Int = base + 5\n}\n",
    )
    .unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed to compile member suspend:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = "import kotlin.coroutines.*;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { }\n\
    };\n\
    Object r = new C(100).m(k);\n\
    System.out.println(r.equals(Integer.valueOf(105)) ? \"OK\" : (\"r=\" + r));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let jc = Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(
        jc.status.success(),
        "javac driver failed:\n{}",
        String::from_utf8_lossy(&jc.stderr)
    );
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "member suspend wrong result; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn suspend_fun_suspension_nested_in_expression() {
    // The suspension `foo()` sits inside a binary expression (`foo() + 2`) at an unconditional
    // position, so it is hoisted to a temp before the expression. 40 + 2 = 42.
    run_suspend(
        "smexpr",
        "suspend fun foo(): Int = 40\n\
         suspend fun e(): Int {\n    val a = foo() + 2\n    return a\n}\n",
        "e",
        42,
    );
}

#[test]
fn suspend_fun_suspension_in_while_loop() {
    // A `while` loop whose body suspends each iteration; `sum`/`i` are loop-carried across the
    // suspension (spilled to continuation fields). 1+1+1 = 3.
    run_suspend(
        "smwhile",
        "suspend fun one(): Int = 1\n\
         suspend fun loopy(): Int {\n\
         \x20   var sum = 0\n\
         \x20   var i = 0\n\
         \x20   while (i < 3) {\n\
         \x20       val x = one()\n\
         \x20       sum = sum + x\n\
         \x20       i = i + 1\n\
         \x20   }\n\
         \x20   return sum\n}\n",
        "loopy",
        3,
    );
}

#[test]
fn suspend_fun_suspension_in_do_while_loop() {
    // A `do`-`while` (post-test) loop whose body suspends: the body runs once before the condition is
    // tested. 1+1+1 = 3.
    run_suspend(
        "smdowhile",
        "suspend fun one(): Int = 1\n\
         suspend fun dw(): Int {\n\
         \x20   var sum = 0\n\
         \x20   var i = 0\n\
         \x20   do {\n\
         \x20       val x = one()\n\
         \x20       sum = sum + x\n\
         \x20       i = i + 1\n\
         \x20   } while (i < 3)\n\
         \x20   return sum\n}\n",
        "dw",
        3,
    );
}

#[test]
fn suspend_fun_suspension_in_if_statement() {
    // The suspension `foo()` is a bare statement inside an `if` STATEMENT branch (not a value). The
    // flattener routes the branch through its own states and converges at the merge. Returns 5.
    run_suspend(
        "smifs",
        "suspend fun foo(): Int = 42\n\
         val flag = true\n\
         suspend fun c(): Int {\n    if (flag) {\n        foo()\n    }\n    return 5\n}\n",
        "c",
        5,
    );
}

#[test]
fn suspend_chain_calls_state_machine_callee() {
    // `top` calls `bar`, which is itself a state-machine suspend fn (it calls `foo`). Exercises a
    // suspend fn whose suspension callee has its own continuation class. 43 + 1 = 44.
    run_suspend(
        "smc",
        "suspend fun foo(): Int = 42\n\
         suspend fun bar(): Int {\n    val a = foo()\n    return a + 1\n}\n\
         suspend fun top(): Int {\n    val x = bar()\n    return x + 1\n}\n",
        "top",
        44,
    );
}

#[test]
fn suspend_fun_calls_member_suspend_fun() {
    // A top-level suspend fn calls a (leaf) member suspend fn `c.leaf()` — the flattener detects the
    // member call (a `MethodCall` to a suspend method) and threads its continuation. 100+5+1 = 106.
    run_suspend(
        "smmembercall",
        "class C(val base: Int) {\n    suspend fun leaf(): Int = base + 5\n}\n\
         suspend fun top(): Int {\n    val c = C(100)\n    val a = c.leaf()\n    return a + 1\n}\n",
        "top",
        106,
    );
}

#[test]
fn suspend_fun_tail_suspension_returns_result() {
    // `h` returns the result of a suspend call directly (`= foo()` → `return foo()`): a tail-position
    // suspension. Desugars to `val tmp = foo(); return tmp` and drives to 42.
    run_suspend(
        "smt",
        "suspend fun foo(): Int = 42\n\
         suspend fun h(): Int = foo()\n",
        "h",
        42,
    );
}

#[test]
fn suspend_fun_discarded_suspension_result() {
    // `g` calls the suspend `sink` for effect (result discarded) — a bare suspend-call statement is a
    // suspension point with no bound local. Then `g` returns 7.
    run_suspend(
        "smd",
        "suspend fun sink(): Int = 0\n\
         suspend fun g(): Int {\n    sink()\n    return 7\n}\n",
        "g",
        7,
    );
}

#[test]
fn suspend_fun_actually_suspends_and_resumes_async() {
    // The ASYNC path (not just synchronous completion): a REAL suspending primitive that returns
    // `COROUTINE_SUSPENDED` and parks its continuation. krusty's caller must propagate
    // `COROUTINE_SUSPENDED` up, and on a later `resumeWith` re-enter its state machine at the resume
    // state and run to completion. `suspendOnce` (kotlinc) parks the continuation; the driver gets
    // `COROUTINE_SUSPENDED` back from `caller`, then resumes with 42 → caller computes 43 and delivers
    // it to the completion. Proves invokeSuspend / label-MIN re-entry actually works.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let (Some(stdlib), Some(kotlinc)) = (stdlib_jar(), kotlinc_bin()) else {
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_async_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // kotlinc builds the suspending primitive: parks the continuation, returns COROUTINE_SUSPENDED.
    fs::write(
        dir.join("Lib.kt"),
        "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
var saved: Continuation<Int>? = null\n\
suspend fun suspendOnce(): Int = suspendCoroutineUninterceptedOrReturn { c ->\n\
    saved = c\n\
    COROUTINE_SUSPENDED\n\
}\n\
fun resumeSaved(v: Int) { saved!!.resumeWith(Result.success(v)) }\n",
    )
    .unwrap();
    let libjar = dir.join("lib.jar");
    let kc = Command::new(&kotlinc)
        .env("JAVA_HOME", &jh)
        .args(["-d", libjar.to_str().unwrap()])
        .arg(dir.join("Lib.kt"))
        .output()
        .unwrap();
    if !kc.status.success() {
        eprintln!(
            "skipping: kotlinc failed:\n{}",
            String::from_utf8_lossy(&kc.stderr)
        );
        return;
    }
    // krusty compiles a caller that suspends on the primitive.
    fs::write(
        dir.join("Use.kt"),
        "suspend fun caller(): Int {\n    val a = suspendOnce()\n    return a + 1\n}\n",
    )
    .unwrap();
    let cp_compile = format!("{}:{}", libjar.to_str().unwrap(), stdlib);
    let ku = Command::new(krusty)
        .args(["-cp", &cp_compile, "-d", dir.to_str().unwrap()])
        .arg(dir.join("Use.kt"))
        .output()
        .unwrap();
    assert!(
        ku.status.success(),
        "krusty failed to compile async suspend caller:\n{}",
        String::from_utf8_lossy(&ku.stderr)
    );
    // Driver: caller suspends (returns COROUTINE_SUSPENDED); resume with 42; completion receives 43.
    let driver = "import kotlin.coroutines.*;\n\
import kotlin.coroutines.intrinsics.IntrinsicsKt;\n\
public class M {\n\
  public static void main(String[] a) {\n\
    final Object[] box = new Object[1];\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { box[0] = o; }\n\
    };\n\
    Object r = UseKt.caller(k);\n\
    boolean suspended = (r == IntrinsicsKt.getCOROUTINE_SUSPENDED());\n\
    LibKt.resumeSaved(42);\n\
    boolean delivered = Integer.valueOf(43).equals(box[0]);\n\
    System.out.println(suspended && delivered ? \"OK\" : (\"suspended=\" + suspended + \" box=\" + box[0]));\n\
  }\n\
}\n";
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!(
        "{}:{}:{}",
        dir.to_str().unwrap(),
        libjar.to_str().unwrap(),
        stdlib
    );
    let jc = Command::new(format!("{jh}/bin/javac"))
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(
        jc.status.success(),
        "javac driver failed:\n{}",
        String::from_utf8_lossy(&jc.stderr)
    );
    let run = Command::new(format!("{jh}/bin/java"))
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "OK",
        "async suspend/resume: wrong result; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}
