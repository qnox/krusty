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

#[test]
fn suspend_fun_with_suspension_point_runs_via_continuation() {
    // `bar` calls the suspend `foo` (a suspension point) so it gets a state machine + continuation
    // class. `foo` completes synchronously (returns a value, never COROUTINE_SUSPENDED), so driving
    // `bar` with a trivial Java `Continuation` runs the whole machine to completion and yields 43.
    // This is the end-to-end proof that the state machine executes correctly.
    let jh = match java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let Some(stdlib) = stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib.jar found");
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_susp_sm_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src = "suspend fun foo(): Int = 42\n\
               suspend fun bar(): Int {\n    val a = foo()\n    return a + 1\n}\n";
    fs::write(dir.join("S.kt"), src).unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "krusty failed to compile suspend state machine:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    // The continuation class must exist.
    assert!(
        dir.join("SKt$bar$1.class").exists(),
        "missing continuation class SKt$bar$1.class"
    );
    // A Java driver: a minimal Continuation whose resumeWith captures the result; call bar(driver).
    let driver = "import kotlin.coroutines.Continuation;\n\
import kotlin.coroutines.CoroutineContext;\n\
import kotlin.coroutines.EmptyCoroutineContext;\n\
public class M {\n\
  static Object captured = null;\n\
  public static void main(String[] a) {\n\
    Continuation<Object> k = new Continuation<Object>() {\n\
      public CoroutineContext getContext() { return EmptyCoroutineContext.INSTANCE; }\n\
      public void resumeWith(Object o) { captured = o; }\n\
    };\n\
    Object r = SKt.bar(k);\n\
    System.out.println(r.equals(Integer.valueOf(43)) ? \"OK\" : (\"r=\" + r));\n\
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
        "state machine produced wrong result; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}
