//! BACKEND-REJECTION coverage: valid Kotlin that the front end accepts but the IR *backend* cleanly
//! DECLINES to lower, emitting a "not yet supported by the IR backend" style diagnostic (a non-zero
//! exit). The box corpus contains only SUPPORTED programs, so these bail branches
//! (`src/jvm/backend.rs`, `src/ir_lower.rs`, `src/jvm/suspend.rs`, `src/jvm/value_classes.rs`) are
//! otherwise never exercised. Each test drives a FULL compile through the krusty binary (front end
//! passes; the backend bails) and asserts the compile is rejected.
//!
//! These are deliberately constructs krusty does NOT support yet — if one of them starts compiling,
//! the feature has landed and the test should be promoted to a real round-trip test elsewhere.

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn java_home() -> Option<String> {
    env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME"))
}

/// Locate a real `kotlin-stdlib.jar` for the compile classpath (unsigned intrinsics, coroutine
/// intrinsics, etc. live there). Mirrors the box harness walk: `target/cache/kotlinc/*/kotlinc/lib`.
fn stdlib_jar() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if let Ok(versions) = fs::read_dir(dir.join("target/cache/kotlinc")) {
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

/// `<stdlib>:<jdk modules>` classpath, or `None` when the toolchain is unavailable (test skips clean).
fn classpath() -> Option<String> {
    let stdlib = stdlib_jar()?;
    let jh = java_home()?;
    let modules = format!("{jh}/lib/modules");
    if !std::path::Path::new(&modules).exists() {
        return None;
    }
    Some(format!("{stdlib}:{modules}"))
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Compile `src` with the krusty binary into a fresh temp dir. Returns `true` when the backend
/// REJECTS the program — a non-zero exit, or a "not (yet) supported" diagnostic on stderr. Returns
/// `true` (skip-clean) when the toolchain is absent, so the suite never fails spuriously on a machine
/// without the vendored kotlinc/JDK.
fn rejects(src: &str) -> bool {
    let Some(cp) = classpath() else {
        return true;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("krusty_reject_{}_{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let kt = dir.join("S.kt");
    fs::write(&kt, src).unwrap();
    let out = Command::new(krusty)
        .args(["-cp", &cp, "-d", dir.to_str().unwrap()])
        .arg(&kt)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    !out.status.success()
        || stderr.contains("not yet supported")
        || stderr.contains("not supported")
}

// --- Unsigned byte/short block-list (src/jvm/ir_emit.rs `ty_ok`/`callee_ok`; emit_all → None; the
//     backend surfaces "this construct is not yet supported by the IR backend"). The corpus never
//     uses `UByte`/`UShort` in a lowered function, so these bail branches are otherwise unreached. ---

#[test]
fn ubyte_conversion_rejected() {
    // `200.toUByte()` returns a `UByte`, a block-listed stdlib value class.
    assert!(rejects(
        "fun main() { val x = 200.toUByte(); println(x) }\n"
    ));
}

#[test]
fn ubyte_literal_conversion_rejected() {
    assert!(rejects("fun main() { println((1).toUByte()) }\n"));
}

#[test]
fn ubyte_parameter_rejected() {
    // A `UByte` parameter type puts the block-listed type into the method descriptor.
    assert!(rejects(
        "fun f(x: UByte): UByte = x\nfun main() { println(f(1.toUByte())) }\n"
    ));
}

#[test]
fn ushort_return_rejected() {
    assert!(rejects(
        "fun g(): UShort = 1.toUShort()\nfun main() { println(g()) }\n"
    ));
}

#[test]
fn ubyte_to_int_rejected() {
    assert!(rejects(
        "fun main() { val b = 5.toUByte(); println(b.toInt()) }\n"
    ));
}

// --- Array-element sized-array constructor (`Array(n) { <array> }`) — src/ir_lower.rs emits the
//     explicit "Array(n) {…} with an array element is not supported" diagnostic. ---

#[test]
fn nested_int_array_ctor_rejected() {
    assert!(rejects(
        "fun main() { val a = Array(2) { IntArray(3) { it } }; println(a[0][0]) }\n"
    ));
}

#[test]
fn triple_nested_array_ctor_rejected() {
    assert!(rejects(
        "fun main() { val a = Array(2) { Array(2) { IntArray(2) } }; println(a.size) }\n"
    ));
}

// --- Mixed spread in a vararg call (`f(0, *a, 3)`) — SpreadBuilder path not modeled; src/ir_lower.rs
//     bails ("call f"), backend surfaces the generic unsupported diagnostic. ---

#[test]
fn mixed_spread_vararg_rejected() {
    assert!(rejects(
        "fun f(vararg xs: Int) = xs.sum()\nfun main() { val a = intArrayOf(1, 2); println(f(0, *a, 3)) }\n"
    ));
}

// --- Delegated properties (`by`) — not lowered yet; src/ir_lower.rs deep-bails. Several distinct
//     delegate providers (custom operator, `lazy`, a `Map`) all take the same bail path. ---

#[test]
fn delegated_property_observable_rejected() {
    assert!(rejects(
        "import kotlin.properties.Delegates\n\
         class C { var x: Int by Delegates.observable(0) { _, _, _ -> } }\n\
         fun main() { val c = C(); c.x = 5; println(c.x) }\n"
    ));
}

#[test]
fn delegated_property_lazy_rejected() {
    assert!(rejects(
        "class C { val x: Int by lazy { 5 } }\nfun main() { println(C().x) }\n"
    ));
}

#[test]
fn delegated_property_map_rejected() {
    assert!(rejects(
        "class C(m: Map<String, Any?>) { val name: String by m }\n\
         fun main() { println(C(mapOf(\"name\" to \"a\")).name) }\n"
    ));
}

// --- Suspend-function shapes the state-machine builder declines (src/jvm/suspend.rs → lower_suspend
//     returns false; backend surfaces "this suspend-function shape is not yet supported"). Each shape
//     exercises a distinct un-handled control-flow construct around a suspension point. ---

#[test]
fn suspend_try_finally_rejected() {
    assert!(rejects(
        "suspend fun d() {}\n\
         suspend fun f() { try { d() } finally { println(\"x\") }; d() }\n"
    ));
}

#[test]
fn suspend_try_catch_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { try { return d() } catch (e: Exception) { return d() } }\n"
    ));
}

#[test]
fn suspend_return_in_try_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { try { return d() } finally {} }\n"
    ));
}

#[test]
fn suspend_try_as_expression_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { val x = try { d() } catch (e: Exception) { 0 }; return x }\n"
    ));
}

#[test]
fn suspend_while_loop_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { var s = 0; while (s < 3) { s += d() }; return s }\n"
    ));
}

#[test]
fn suspend_do_while_loop_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { var i = 0; do { i += d() } while (i < 3); return i }\n"
    ));
}

#[test]
fn suspend_for_loop_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { var s = 0; for (i in 0..2) { s += d() }; return s }\n"
    ));
}

#[test]
fn suspend_when_with_multiple_suspensions_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(x: Int): Int = when (x) { 0 -> d(); else -> d() + d() }\n"
    ));
}

#[test]
fn suspend_safe_call_double_suspension_rejected() {
    assert!(rejects(
        "class Box { suspend fun d(): Int = 1 }\n\
         suspend fun f(b: Box?): Int { return (b?.d() ?: 0) + (b?.d() ?: 0) }\n"
    ));
}
