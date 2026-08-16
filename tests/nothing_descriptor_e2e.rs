//! `Nothing` is uninhabited, so no value ever carries its descriptor — but it IS written into
//! signatures, and kotlinc writes `java.lang.Void` there. krusty wrote `java.lang.Object`, so a
//! `fun fail(): Nothing` helper — which intellij-community and the Kotlin stdlib are full of — had a
//! different descriptor from the one callers compiled against kotlinc link to.

use super::common;
use std::fs;

fn disassemble(name: &str, classes: &[(String, Vec<u8>)], class: &str) -> String {
    let dir = std::env::temp_dir().join(format!("krusty_nothing_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create Nothing descriptor output");
    for (internal, bytes) in classes {
        let path = dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create class package output");
        }
        fs::write(&path, bytes).expect("write emitted class");
    }
    let text = common::javap(&[
        "-c",
        "-p",
        "-s",
        &dir.join(format!("{class}.class")).to_string_lossy(),
    ])
    .expect("javap Nothing descriptor output");
    let _ = fs::remove_dir_all(&dir);
    text
}

/// Compile with the stdlib on the classpath and disassemble the facade with `javap`.
fn facade_disassembly(name: &str, source: &str) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        source,
        "Nothings",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("compile Nothing descriptor source");
    disassemble(name, &classes, "NothingsKt")
}

#[test]
fn a_function_returning_nothing_returns_void_in_its_descriptor() {
    let text = facade_disassembly(
        "ret",
        "fun fail(message: String): Nothing = throw IllegalStateException(message)\n",
    );
    assert!(
        text.contains("(Ljava/lang/String;)Ljava/lang/Void;"),
        "kotlinc returns java/lang/Void for a Nothing result:\n{text}"
    );
}

#[test]
fn a_nothing_getter_returns_void_in_its_descriptor() {
    let text = facade_disassembly(
        "getter",
        "val broken: Nothing get() = throw IllegalStateException(\"x\")\n",
    );
    assert!(
        text.contains("()Ljava/lang/Void;"),
        "a Nothing getter returns java/lang/Void:\n{text}"
    );
}

#[test]
fn a_nothing_parameter_uses_void_in_its_descriptor() {
    let text = facade_disassembly("parameter", "fun consume(value: Nothing) {}\n");
    assert!(
        text.contains("(Ljava/lang/Void;)V"),
        "a Nothing parameter uses java/lang/Void:\n{text}"
    );
}

#[test]
fn a_nullable_nothing_result_uses_void_in_its_descriptor() {
    let text = facade_disassembly("nullable_result", "fun maybe(): Nothing? = null\n");
    assert!(
        text.contains("()Ljava/lang/Void;"),
        "a Nothing? result uses java/lang/Void:\n{text}"
    );
}

#[test]
fn an_inferred_nullable_nothing_result_uses_void_in_its_descriptor() {
    let text = facade_disassembly("inferred_nullable_result", "fun maybe() = null\n");
    assert!(
        text.contains("()Ljava/lang/Void;"),
        "an inferred Nothing? result uses java/lang/Void:\n{text}"
    );
}

#[test]
fn inferred_and_explicit_nullable_nothing_properties_link_their_void_fields() {
    let source = r#"
        val inferred = null
        val explicit: Nothing? = null

        class Holder {
            val inferred = null
            val explicit: Nothing? = null
        }

        fun box(): String {
            val holder = Holder()
            return if (
                inferred == null && explicit == null &&
                holder.inferred == null && holder.explicit == null
            ) "OK" else "F"
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
        Some("OK")
    );
}

#[test]
fn member_and_local_declarations_use_the_same_void_descriptor() {
    let source = r#"
        class Holder(val stored: Nothing) {
            fun member(value: Nothing): Nothing? = null
        }

        fun outer() {
            fun local(value: Nothing): Nothing? = null
        }
    "#;
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        source,
        "Nothings",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("compile member and local Nothing declarations");
    let member = disassemble("member", &classes, "Holder");
    let facade = disassemble("local", &classes, "NothingsKt");
    let descriptor = "(Ljava/lang/Void;)Ljava/lang/Void;";
    assert!(member.contains(descriptor), "member descriptor:\n{member}");
    assert!(
        member.contains("descriptor: Ljava/lang/Void;"),
        "backing-field descriptor:\n{member}"
    );
    assert!(
        member.contains("(Ljava/lang/Void;)V"),
        "constructor descriptor:\n{member}"
    );
    assert!(facade.contains(descriptor), "local descriptor:\n{facade}");
}

#[test]
fn sibling_module_call_uses_the_declared_void_descriptor() {
    let sources = [
        (
            "Library.kt",
            "fun source(value: Nothing): Nothing? = null\n",
        ),
        (
            "Consumer.kt",
            "fun proxy(value: Nothing): Nothing? = source(value)\n",
        ),
    ];
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &sources,
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("compile sibling-module Nothing call");
    let consumer = classes
        .iter()
        .map(|(internal, _)| internal.as_str())
        .find(|internal| internal.contains("Consumer") && !internal.contains('$'))
        .expect("consumer facade");
    let text = disassemble("module", &classes, consumer);
    assert!(
        text.contains("source:(Ljava/lang/Void;)Ljava/lang/Void;"),
        "module call descriptor:\n{text}"
    );
}

#[test]
fn classpath_call_uses_metadata_nothing_without_an_origin_branch() {
    let Some(library) = common::compile_lib(
        "nothing_descriptor_classpath",
        "package lib\nfun source(value: Nothing): Nothing? = null\n",
    ) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        "import lib.source\nfun proxy(value: Nothing): Nothing? = source(value)\n",
        "Nothings",
        &[library, stdlib],
        Some(jdk.as_path()),
    )
    .expect("compile classpath Nothing call");
    let text = disassemble("classpath", &classes, "NothingsKt");
    assert!(
        text.contains("source:(Ljava/lang/Void;)Ljava/lang/Void;"),
        "classpath call descriptor:\n{text}"
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
