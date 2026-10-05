//! A private property reference reports the accessor reflection looks up, and calls the bridge
//! that actually exists.
//!
//! A top-level value-class `val` is stored as its carrier. Reflection synthesizes `getOk()` of
//! that carrier from the field descriptor, and the reference calls `access$getOk$p`. A companion
//! `val` is a static of the outer class, so the same reference calls `access$getOk$cp` there —
//! including a plain `String`, which has no value-class mangling of its own. A companion `var`
//! reads and writes that static through `access$getOk$cp` / `access$setOk$cp`. A reference
//! carrier is unboxed; a nullable primitive carrier stays boxed.
use std::path::{Path, PathBuf};

use super::common::{self, compile_and_run_box_files};

fn reflect_jar() -> PathBuf {
    common::dist_jar("kotlin-reflect.jar").unwrap_or_else(|| {
        panic!("kotlin-reflect.jar is required to call a private property");
    })
}

fn agree(source: &str) {
    let reflect = reflect_jar();
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty = compile_and_run_box_files(
        &[("main.kt", source)],
        &[stdlib.clone(), reflect.clone()],
        Some(jdk.as_path()),
    )
    .expect("krusty box");
    let kotlinc = kotlinc_box(source, &stdlib, &reflect);
    assert_eq!(krusty, kotlinc);
    assert_eq!(krusty, "OK");
}

fn kotlinc_box(source: &str, stdlib: &Path, reflect: &Path) -> String {
    let work = common::scratch_dir().expect("scratch");
    let source_path = work.join("main.kt");
    std::fs::write(&source_path, source).expect("write source");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("output dir");
    let (code, diagnostics) = common::kotlinc_compile(&[
        source_path.display().to_string(),
        "-d".to_string(),
        output.display().to_string(),
    ])
    .expect("kotlinc");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    let result = common::run_box(
        &[],
        "MainKt",
        &[output, stdlib.to_path_buf(), reflect.to_path_buf()],
    )
    .expect("kotlinc box");
    let _ = std::fs::remove_dir_all(work);
    result
}

#[test]
fn a_private_top_level_value_class_property_is_callable() {
    agree(
        r#"
import kotlin.reflect.jvm.isAccessible

inline class S(val s: String)

private val ok = S("OK")

fun box() = ::ok.apply { isAccessible = true }.call().s
"#,
    );
}

#[test]
fn a_private_companion_value_class_property_is_callable() {
    agree(
        r#"
import kotlin.reflect.jvm.isAccessible

inline class S(val s: String)

class Host {
    companion object {
        private val ok = S("OK")
        val ref = ::ok.apply { isAccessible = true }
    }
}

fun box() = Host.ref.call().s
"#,
    );
}

#[test]
fn a_private_companion_string_property_is_callable() {
    agree(
        r#"
import kotlin.reflect.jvm.isAccessible

class Host {
    companion object {
        private val ok = "OK"
        val ref = ::ok.apply { isAccessible = true }
    }
}

fun box() = Host.ref.call()
"#,
    );
}

#[test]
fn a_private_companion_value_class_var_is_readable_and_writable() {
    agree(
        r#"
import kotlin.reflect.jvm.isAccessible

inline class S(val s: String)

class Host {
    companion object {
        private var ok = S("no")
        val ref = ::ok.apply { isAccessible = true }
    }
}

fun box(): String {
    Host.ref.set(S("OK"))
    return Host.ref.get().s
}
"#,
    );
}

#[test]
fn a_private_companion_nullable_primitive_var_stays_boxed() {
    agree(
        r#"
import kotlin.reflect.jvm.isAccessible

inline class I(val n: Int)

class Host {
    companion object {
        private var ok: I? = null
        val ref = ::ok.apply { isAccessible = true }
    }
}

fun box(): String {
    Host.ref.set(I(7))
    return if (Host.ref.get()?.n == 7) "OK" else "fail"
}
"#,
    );
}
