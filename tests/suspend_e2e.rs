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
