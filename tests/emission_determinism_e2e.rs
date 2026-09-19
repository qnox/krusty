//! Emission determinism: the same sources compiled repeatedly must produce byte-identical
//! artifacts, and the ways in which output legitimately depends on its INPUT ORDER are pinned here.
//!
//! Why this is a gate rather than a measurement. `docs/BUILD_AND_NATIVE_PLAN.md` proposes a
//! content-addressed build cache keyed on artifact hashes. Such a cache over a nondeterministic
//! producer inverts: an unchanged module's hash changes on rebuild, so every dependent rebuilds
//! rather than none. Worse, it then HIDES the defect — a nondeterminism bug that would otherwise
//! surface as a byte diff instead surfaces as a cache hit.
//!
//! Two historical reports recorded nondeterministic emission — `docs/RESOLUTION_ENGINE_PLAN.md`
//! (four distinct md5s for `unqualifiedSuperKt$box$1.class` across six runs) and `docs/SPEC.md` (a
//! class differing "between any two runs of the SAME binary"). Neither reproduces today; both
//! predate the FIR streaming migration. These tests keep it that way.
//!
//! Rust seeds each `HashMap`'s `RandomState` differently within one process, so repeated in-process
//! compilation exposes hash-iteration-order leaks the same way repeated process runs do, without
//! paying a cold classpath scan per run.

use super::common;

/// Compilations per determinism assertion. Iteration order differs per `HashMap` instance, so a
/// leak shows up quickly; this is a gate on every test run, so it stays cheap.
const RUNS: usize = 8;

/// Exercises the shapes the historical reports implicated: unqualified `super` through multiple
/// interface defaults, object expressions (the reported class was `…Kt$box$1`), lambdas, and enough
/// members that a class signature's member map has room to reorder.
const RICH: &str = r#"
interface A {
    fun foo(): String = "A"
    val bar: String get() = "barA"
}
interface B {
    fun foo(): String = "B"
    val baz: String get() = "bazB"
}
open class Base {
    open fun greet(): String = "base"
}
class C : Base(), A, B {
    override fun foo(): String = super<A>.foo() + super<B>.foo()
    override fun greet(): String = super.greet() + "-c"
    val m1: Int = 1
    val m2: String = "two"
    var m3: Int = 3
    fun m4(x: Int, y: String): String = y + (x + m1)
    fun m5(x: String): Int = x.length + m3
}
fun box(): String {
    val c = C()
    val anon = object : A {
        override fun foo(): String = "anon" + c.foo()
    }
    val inc = { x: Int -> x + 1 }
    val len = { s: String -> s.length }
    val total = inc(1) + len("ab") + anon.foo().length + c.greet().length + c.m4(1, "z").length
    return if (total > 0) "OK" else "FAIL"
}
"#;

/// `(name, bytes)` for every artifact, in emission order.
fn compile_once(classpath: &[std::path::PathBuf], jdk: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    common::compile_in_process(RICH, "Main", classpath, Some(jdk)).unwrap_or_else(|| {
        panic!(
            "determinism fixture must compile:\n{:?}",
            common::front_end_diagnostics(RICH, classpath, Some(jdk))
        )
    })
}

/// Repeated compilation of one source is byte-identical, artifact order included.
///
/// Artifact ORDER is asserted alongside the bytes: a build cache that stored artifacts in emission
/// order would hash differently under a reordering even with every individual class unchanged.
#[test]
fn repeated_compilation_is_byte_identical() {
    let jdk = common::jdk_modules();
    let classpath = vec![common::stdlib_jar()];
    let first = compile_once(&classpath, jdk.as_path());
    assert!(
        first.iter().any(|(name, _)| name.contains("$box$")),
        "fixture must emit an object-expression class — the shape the historical report \
         implicated; emitted: {:?}",
        first.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );

    for run in 2..=RUNS {
        let next = compile_once(&classpath, jdk.as_path());
        assert_eq!(
            first.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            next.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "artifact names/order differ between run 1 and run {run}"
        );
        for ((name, want), (_, got)) in first.iter().zip(&next) {
            assert_eq!(
                want,
                got,
                "run {run} emitted different bytes for {name} ({} vs {} bytes)",
                want.len(),
                got.len()
            );
        }
    }
}

/// The same property across a multi-file source set, where module finalization (`.kotlin_module`)
/// also participates.
#[test]
fn repeated_multi_file_compilation_is_byte_identical() {
    let jdk = common::jdk_modules();
    let classpath = vec![common::stdlib_jar()];
    let sources: &[(&str, &str)] = &[
        (
            "Delta.kt",
            "package shared\nfun deltaTop(): String = \"Delta\"\n",
        ),
        (
            "Echo.kt",
            "package shared\nfun echoTop(): String = \"Echo\"\n",
        ),
        (
            "Foxtrot.kt",
            "package shared\nclass Fox { fun go(): String = \"fox\" }\n",
        ),
        ("Zulu.kt", "package shared\nfun box(): String = \"OK\"\n"),
    ];
    let compile = || {
        common::compile_in_process_files(sources, &classpath, Some(jdk.as_path()))
            .expect("multi-file determinism fixture must compile")
    };

    let first = compile();
    assert!(
        first
            .iter()
            .any(|(name, _)| name.ends_with(".kotlin_module")),
        "fixture must emit a .kotlin_module so finalization is covered; emitted: {:?}",
        first.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    for run in 2..=RUNS {
        let next = compile();
        assert_eq!(first, next, "multi-file run {run} differs from run 1");
    }
}

/// `.kotlin_module` depends on SOURCE ORDER: a package's facade-name list accumulates in
/// file-streaming order (`JvmState::module_packages` holds `BTreeMap<String, Vec<String>>` — the
/// packages are ordered, the facade names within one package are not sorted).
///
/// This is input-order sensitivity, not nondeterminism, and it is pinned because
/// `docs/BUILD_AND_NATIVE_PLAN.md`'s cache key depends on it: the key hashes an ORDERED source
/// list, and a key over a sorted multiset would collide two builds whose `.kotlin_module` bytes
/// genuinely differ. Class files are unaffected and are asserted identical here.
///
/// If emission is ever changed to sort facade names, this test fails — at which point the cache key
/// may be narrowed to an order-independent source set. Update both together.
#[test]
fn module_facade_order_follows_source_order_but_classes_do_not() {
    let jdk = common::jdk_modules();
    let classpath = vec![common::stdlib_jar()];
    let forward: &[(&str, &str)] = &[
        (
            "Delta.kt",
            "package shared\nfun deltaTop(): String = \"Delta\"\n",
        ),
        (
            "Echo.kt",
            "package shared\nfun echoTop(): String = \"Echo\"\n",
        ),
        ("Zulu.kt", "package shared\nfun box(): String = \"OK\"\n"),
    ];
    let reversed: Vec<(&str, &str)> = forward.iter().rev().copied().collect();

    let a = common::compile_in_process_files(forward, &classpath, Some(jdk.as_path()))
        .expect("forward order must compile");
    let b = common::compile_in_process_files(&reversed, &classpath, Some(jdk.as_path()))
        .expect("reversed order must compile");

    let module_of = |artifacts: &[(String, Vec<u8>)]| -> Vec<u8> {
        artifacts
            .iter()
            .find(|(name, _)| name.ends_with(".kotlin_module"))
            .map(|(_, bytes)| bytes.clone())
            .expect("a .kotlin_module artifact")
    };
    assert_ne!(
        module_of(&a),
        module_of(&b),
        ".kotlin_module is expected to track source order; if this now matches, emission started \
         sorting facade names and BUILD_AND_NATIVE_PLAN.md's cache key can drop source ORDER"
    );

    // Every class file is order-independent: only module finalization sees the streaming order.
    let classes = |artifacts: &[(String, Vec<u8>)]| -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<(String, Vec<u8>)> = artifacts
            .iter()
            .filter(|(name, _)| !name.ends_with(".kotlin_module"))
            .cloned()
            .collect();
        v.sort_by(|x, y| x.0.cmp(&y.0));
        v
    };
    assert_eq!(
        classes(&a),
        classes(&b),
        "class files must not depend on source order"
    );
}
