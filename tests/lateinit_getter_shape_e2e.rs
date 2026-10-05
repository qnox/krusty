//! A `lateinit` property's getter takes kotlinc's `LateinitLowering` shape: return the field when it
//! is set, otherwise throw and fall through to `aconst_null; areturn`:
//! `dup; ifnull L; areturn; L: pop; ldc name; invokestatic throwUninitialized…; aconst_null; areturn`.
//! A member getter used the inline read guard instead (`dup; ifnonnull L; ldc; invokestatic; L:
//! areturn`), which a facade getter already did not.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and each
//! getter's instructions are compared exactly.
use std::fs;

use super::common;

const SOURCE: &str = r#"
lateinit var top: String

class Holder {
    lateinit var name: String
    lateinit var items: List<String>
}

object Single { lateinit var value: String }

enum class Kind { A; lateinit var label: String }

fun box(): String {
    top = "t"; Single.value = "v"; Kind.A.label = "k"
    val h = Holder()
    val missing = try { h.name; "set" } catch (e: UninitializedPropertyAccessException) { e.message }
    h.name = "n"; h.items = listOf("i")
    val r = top + h.name + h.items[0] + Single.value + Kind.A.label + missing
    return if (r == "tnivklateinit property name has not been initialized") "OK" else r
}
"#;

/// `method`'s instructions in `class`, constant-pool indices dropped.
fn body(dir: &std::path::Path, class: &str, method: &str) -> Vec<String> {
    let path = dir.join(format!("{class}.class"));
    common::javap(&["-c", "-p", &path.to_string_lossy()])
        .expect("pooled javap")
        .lines()
        .skip_while(|line| !line.contains(&format!(" {method}();")))
        .skip(2)
        .take_while(|line| !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .filter(|token| !token.starts_with('#'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn a_lateinit_getter_returns_or_throws_like_kotlinc() {
    let base = std::env::temp_dir().join(format!("krusty_lateinit_getter_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let krusty_dir = base.join("krusty");
    let kotlinc_dir = base.join("kotlinc");
    fs::create_dir_all(&krusty_dir).unwrap();
    fs::create_dir_all(&kotlinc_dir).unwrap();
    let source = base.join("Late.kt");
    fs::write(&source, SOURCE).unwrap();
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let classes = common::compile_in_process(
        SOURCE,
        "Late",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty failed to compile the fixture");
    for (internal, bytes) in &classes {
        fs::write(krusty_dir.join(format!("{internal}.class")), bytes).unwrap();
    }
    for (class, getter) in [
        ("LateKt", "getTop"),
        ("Holder", "getName"),
        ("Holder", "getItems"),
        ("Single", "getValue"),
        ("Kind", "getLabel"),
    ] {
        let expected = body(&kotlinc_dir, class, getter);
        assert!(
            expected.iter().any(|line| line.ends_with("aconst_null")),
            "{class}.{getter}: kotlinc's getter body"
        );
        assert_eq!(
            body(&krusty_dir, class, getter),
            expected,
            "{class}.{getter} must match kotlinc's"
        );
    }
}

#[test]
fn a_lateinit_getter_throws_until_assigned() {
    assert_eq!(
        common::compile_and_run_box(
            SOURCE,
            "Late",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path())
        )
        .as_deref(),
        Some("OK")
    );
}
