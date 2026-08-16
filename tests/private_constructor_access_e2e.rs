//! A `private constructor` is `ACC_PRIVATE`, as kotlinc emits it.
//!
//! krusty made every constructor public, so `class C private constructor()` advertised a constructor
//! its own author had hidden and any caller could invoke it. The reason was real but only covers one
//! case: when ANOTHER class constructs the type (a companion factory is the common shape), kotlinc
//! reaches the private constructor through a synthetic `DefaultConstructorMarker` bridge, and without
//! that bridge `ACC_PRIVATE` would turn the cross-class `new` into an `IllegalAccessError`. So the
//! flag now follows the actual construction sites: private when nothing outside constructs it, public
//! (as before) when something does, until the bridge lands.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty.
use std::fs;

use super::common;

/// `<init>` entries as `javap -p` prints them, in order — the declaration line carries the access
/// keyword, which is the fact under test.
fn constructors(dir: &std::path::Path, class: &str) -> Vec<String> {
    let path = dir.join(format!("{class}.class"));
    let raw = common::javap(&["-p", &path.to_string_lossy()]).expect("pooled javap");
    raw.lines()
        .map(str::trim)
        .filter(|line| line.starts_with(class) || line.contains(&format!(" {class}(")))
        .map(str::to_string)
        .collect()
}

/// Compile `src` with BOTH compilers; `None` when the provisioned toolchain is unavailable.
fn compile_both(tag: &str, src: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    // Per-TAG directory: these tests run in parallel and would otherwise share one output tree.
    let base = std::env::temp_dir().join(format!("krusty_priv_ctor_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let krusty_dir = base.join("krusty");
    let kotlinc_dir = base.join("kotlinc");
    fs::create_dir_all(&krusty_dir).ok()?;
    fs::create_dir_all(&kotlinc_dir).ok()?;

    let source = base.join("Ctors.kt");
    fs::write(&source, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().to_string(),
        "-d".to_string(),
        kotlinc_dir.to_string_lossy().to_string(),
    ])?;
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");

    let classes = common::compile_in_process(
        src,
        "Ctors",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty failed to compile the fixture");
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(&path, bytes).ok()?;
    }
    Some((krusty_dir, kotlinc_dir))
}

#[test]
fn a_private_constructor_nobody_constructs_from_outside_is_private() {
    let src = r#"
class Hidden private constructor() {
    fun self(): Hidden = Hidden()
}
class Plain(val x: Int)
"#;
    let Some((krusty_dir, kotlinc_dir)) = compile_both("alone", src) else {
        return; // toolchain not provisioned
    };
    for class in ["Hidden", "Plain"] {
        assert_eq!(
            constructors(&krusty_dir, class),
            constructors(&kotlinc_dir, class),
            "{class}: constructor access must match kotlinc's"
        );
    }
    // Guard the comparison — an all-public pair would otherwise pass vacuously.
    assert!(
        constructors(&krusty_dir, "Hidden")
            .iter()
            .any(|entry| entry.starts_with("private")),
        "the hidden constructor must be private: {:?}",
        constructors(&krusty_dir, "Hidden")
    );
}

#[test]
fn a_private_constructor_reached_from_another_class_stays_public() {
    // Until the `DefaultConstructorMarker` bridge is emitted, a class its companion constructs keeps
    // a public constructor: ACC_PRIVATE without the bridge is an IllegalAccessError at run time. The
    // class must therefore still LOAD and construct.
    let src = r#"
class Made private constructor(val x: Int) {
    companion object {
        fun make(): Made = Made(7)
    }
}
fun box(): String = if (Made.make().x == 7) "OK" else "FAIL"
"#;
    let Some((krusty_dir, _kotlinc_dir)) = compile_both("companion", src) else {
        return; // toolchain not provisioned
    };
    assert!(
        constructors(&krusty_dir, "Made")
            .iter()
            .all(|entry| !entry.starts_with("private")),
        "a constructor reached from the companion must stay public until the bridge exists: {:?}",
        constructors(&krusty_dir, "Made")
    );
    // And it must still RUN — the flag choice exists to keep the cross-class construction legal.
    assert_eq!(
        common::compile_and_run_box(
            src,
            "Ctors",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path())
        )
        .as_deref(),
        Some("OK")
    );
}

/// A DEFAULT ARGUMENT is evaluated at the call site, not in the declaring body, so a nested class
/// whose parameter defaults to `Hidden2(…)` constructs the private constructor from a DIFFERENT JVM
/// class. Reaching that expression means walking more than function bodies; missing it made krusty
/// emit `ACC_PRIVATE` with no bridge, and the class died with `IllegalAccessError` at run time.
#[test]
fn a_default_argument_in_a_nested_class_counts_as_an_outside_construction() {
    let src = r#"
class Hidden2 private constructor(val x: Int) {
    class Maker {
        fun make(h: Hidden2 = Hidden2(1)): Int = h.x
    }
    companion object {
        fun viaSecondary(): Int = Wrap().x
        class Wrap {
            val x: Int
            constructor(h: Hidden2 = Hidden2(2)) { x = h.x }
        }
    }
}
fun box(): String = if (Hidden2.Maker().make() == 1 && Hidden2.viaSecondary() == 2) "OK" else "FAIL"
"#;
    let Some((krusty_dir, _kotlinc_dir)) = compile_both("nested_default", src) else {
        return; // toolchain not provisioned
    };
    assert!(
        constructors(&krusty_dir, "Hidden2")
            .iter()
            .all(|entry| !entry.starts_with("private")),
        "a constructor reached from a nested class's default argument must stay public: {:?}",
        constructors(&krusty_dir, "Hidden2")
    );
    assert_eq!(
        common::compile_and_run_box(
            src,
            "Ctors",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path())
        )
        .as_deref(),
        Some("OK")
    );
}

/// A SUBCLASS reaches the constructor without constructing the type: its own `<init>` calls
/// `invokespecial <super>.<init>` from a different JVM class, which is the same private access.
#[test]
fn a_nested_subclass_counts_as_an_outside_construction() {
    let src = r#"
open class Base3 private constructor(val x: Int) {
    class Sub : Base3(5)
    companion object { fun make(): Base3 = Sub() }
}
fun box(): String = if (Base3.make().x == 5) "OK" else "FAIL"
"#;
    let Some((krusty_dir, _kotlinc_dir)) = compile_both("subclass", src) else {
        return; // toolchain not provisioned
    };
    assert!(
        constructors(&krusty_dir, "Base3")
            .iter()
            .all(|entry| !entry.starts_with("private")),
        "a constructor a subclass delegates to must stay public: {:?}",
        constructors(&krusty_dir, "Base3")
    );
    assert_eq!(
        common::compile_and_run_box(
            src,
            "Ctors",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path())
        )
        .as_deref(),
        Some("OK")
    );
}
