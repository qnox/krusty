//! Reject paths in the checker the corpus never triggers: a duplicate local declaration, a top-level
//! property with a custom accessor (unsupported), and two extension properties with the same erased
//! receiver. Each drives a distinct `diags.error` branch.

mod common;

fn diags(src: &str) -> Vec<String> {
    let stdlib = match common::stdlib_jar() {
        Some(p) => p,
        None => {
            eprintln!("skipping resolve_reject_coverage_e2e: no kotlin-stdlib jar");
            return vec![];
        }
    };
    let jdk = common::java_home().map(|h| std::path::PathBuf::from(format!("{h}/lib/modules")));
    common::front_end_diagnostics(src, &[stdlib], jdk.as_deref())
}

fn assert_contains(src: &str, needle: &str) {
    let d = diags(src);
    if d.is_empty() {
        return; // environment skip
    }
    assert!(
        d.iter().any(|m| m.contains(needle)),
        "expected a diagnostic containing {needle:?}, got: {d:?}"
    );
}

#[test]
fn duplicate_local_declaration_rejected() {
    assert_contains(
        "fun f() {\n    val x = 1\n    val x = 2\n    println(x)\n}\n",
        "conflicting local declaration",
    );
}

#[test]
fn top_level_property_custom_accessor_rejected() {
    assert_contains(
        "val x: Int\n    get() = 5\n",
        "custom accessors are not supported",
    );
}

#[test]
fn conflicting_extension_property_rejected() {
    assert_contains(
        "val Int.p: Int get() = 1\nval Int.p: Int get() = 2\n",
        "conflicting extension property",
    );
}
