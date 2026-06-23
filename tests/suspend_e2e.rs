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
