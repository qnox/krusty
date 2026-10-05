//! An override inherits its parameters' defaults, but only the function that declared them gets a
//! `name$default` synthetic. kotlinc routes every omitted-argument call through the declaring owner's
//! stub; krusty emitted a second, redundant stub on each overriding class, interface and entry body.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and every
//! class's `javap -p` member list is compared exactly.
use std::fs;

use super::common;

const SOURCE: &str = r#"
abstract class Base { abstract fun foo(a: String = "abc"): String }
open class Derived : Base() { override fun foo(a: String): String = a }
class Leaf : Derived() { override fun foo(a: String): String = a + "!" }
interface I { fun bar(x: Int = 1): Int }
class Impl : I { override fun bar(x: Int): Int = x }
interface J : I { override fun bar(x: Int): Int = x + 1 }
object Single : I { override fun bar(x: Int): Int = x * 2 }
enum class Kind { A { override fun m(a: Int): Int = a + 10 }, B; open fun m(a: Int = 3): Int = a }
fun box(): String {
    val j = object : J {}
    val r = Derived().foo() + Leaf().foo() + Impl().bar() + j.bar() + Single.bar() + Kind.A.m() + Kind.B.m()
    return if (r == "abcabc!122133") "OK" else r
}
"#;

/// `javap -p` of one class, with the `Compiled from` header dropped.
fn members(dir: &std::path::Path, class: &str) -> Vec<String> {
    let path = dir.join(format!("{class}.class"));
    common::javap(&["-p", &path.to_string_lossy()])
        .expect("pooled javap")
        .lines()
        .filter(|line| !line.starts_with("Compiled from"))
        .map(str::to_string)
        .collect()
}

#[test]
fn only_the_declaring_function_gets_a_default_stub() {
    let base = std::env::temp_dir().join(format!("krusty_override_default_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let krusty_dir = base.join("krusty");
    let kotlinc_dir = base.join("kotlinc");
    fs::create_dir_all(&krusty_dir).unwrap();
    fs::create_dir_all(&kotlinc_dir).unwrap();
    let source = base.join("Overrides.kt");
    fs::write(&source, SOURCE).unwrap();
    let Some((code, stderr)) = common::kotlinc_compile(&[
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ]) else {
        return; // toolchain not provisioned
    };
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let classes = common::compile_in_process(
        SOURCE,
        "Overrides",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty failed to compile the fixture");
    for (internal, bytes) in &classes {
        fs::write(krusty_dir.join(format!("{internal}.class")), bytes).unwrap();
    }
    // `Kind` itself is left out: its constructor's visibility is a separate enum gap.
    for class in [
        "Base",
        "Derived",
        "Leaf",
        "I",
        "I$DefaultImpls",
        "Impl",
        "J",
        "J$DefaultImpls",
        "Single",
        "Kind$A",
        "OverridesKt",
    ] {
        assert_eq!(
            members(&krusty_dir, class),
            members(&kotlinc_dir, class),
            "{class}: the member list must match kotlinc's"
        );
    }
}

/// Omitted-argument calls through every override shape still fill the declared defaults.
#[test]
fn calls_through_an_override_use_the_declared_defaults() {
    assert_eq!(
        common::compile_and_run_box(
            SOURCE,
            "Overrides",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path())
        )
        .as_deref(),
        Some("OK")
    );
}
