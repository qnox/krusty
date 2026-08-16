//! `Nothing` is uninhabited, so no value ever carries its descriptor — but it IS written into
//! signatures, and kotlinc writes `java.lang.Void` there. krusty wrote `java.lang.Object`, so a
//! `fun fail(): Nothing` helper — which intellij-community and the Kotlin stdlib are full of — had a
//! different descriptor from the one callers compiled against kotlinc link to.
//!
//! A `Nothing` PARAMETER (`fun f(n: Nothing)`) still descriptors as `Object`; it reaches the backend
//! already erased. See `docs/SPEC.md`.

use super::common;
use std::fs;

/// Compile with the stdlib on the classpath and disassemble the facade with `javap`.
fn facade_disassembly(name: &str, source: &str) -> Option<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        source,
        "Nothings",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )?;
    let dir = std::env::temp_dir().join(format!("krusty_nothing_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).ok()?;
    for (internal, bytes) in &classes {
        let path = dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(&path, bytes).ok()?;
    }
    let text = common::javap(&["-p", "-s", &dir.join("NothingsKt.class").to_string_lossy()]);
    let _ = fs::remove_dir_all(&dir);
    text
}

#[test]
fn a_function_returning_nothing_returns_void_in_its_descriptor() {
    let Some(text) = facade_disassembly(
        "ret",
        "fun fail(message: String): Nothing = throw IllegalStateException(message)\n",
    ) else {
        return;
    };
    assert!(
        text.contains("(Ljava/lang/String;)Ljava/lang/Void;"),
        "kotlinc returns java/lang/Void for a Nothing result:\n{text}"
    );
}

#[test]
fn a_nothing_getter_returns_void_in_its_descriptor() {
    let Some(text) = facade_disassembly(
        "getter",
        "val broken: Nothing get() = throw IllegalStateException(\"x\")\n",
    ) else {
        return;
    };
    assert!(
        text.contains("()Ljava/lang/Void;"),
        "a Nothing getter returns java/lang/Void:\n{text}"
    );
}

#[test]
fn a_function_returning_nothing_still_throws_at_runtime() {
    let source = r#"
        fun fail(): Nothing = throw IllegalStateException("boom")

        fun box(): String = try {
            fail()
        } catch (e: IllegalStateException) {
            e.message ?: "no message"
        }
    "#;

    assert_eq!(
        common::compile_and_run_box(
            source,
            "Main",
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        )
        .as_deref(),
        Some("boom")
    );
}
