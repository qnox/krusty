//! Harness semantics: `compile_libs` is content-addressed. The same source set must resolve to the
//! SAME on-disk classpath dir (so repeated tests share one kotlinc compile and one box-runner
//! classpath), and a different source set must resolve to a different dir. These are the invariants
//! the e2e suite's wall-clock depends on — without them every lib test pays a fresh reference-compiler
//! round-trip and spawns its own runner JVM.
//!
//! These tests use the strict [`common::Fixture`] / panic-on-missing-toolchain contract: a
//! misconfigured environment fails them, it does not let them "pass" as skips.

use super::common;

fn expect_lib(tag: &str, src: &str) -> std::path::PathBuf {
    common::compile_lib(tag, src).unwrap_or_else(|| {
        panic!(
            "{tag}: reference kotlinc dist is not provisioned; run `just kotlinc \"$(just max-version)\"`"
        )
    })
}

#[test]
fn same_lib_source_reuses_cached_classpath_dir() {
    let src = "package cachedemo\nfun seed(): Int = 41\n";
    let first = expect_lib("libcache_same", src);
    let second = expect_lib("libcache_same", src);
    assert_eq!(
        first, second,
        "identical lib sources must map to one cached classpath dir"
    );
    assert!(
        std::fs::read_dir(&first)
            .map(|d| d.count() > 0)
            .unwrap_or(false),
        "cached classpath dir must contain compiled classes"
    );
}

#[test]
fn different_lib_source_gets_distinct_classpath_dir() {
    let dir_a = expect_lib(
        "libcache_diff_a",
        "package cachedemo\nfun seedA(): Int = 1\n",
    );
    let dir_b = expect_lib(
        "libcache_diff_b",
        "package cachedemo\nfun seedB(): Int = 2\n",
    );
    assert_ne!(
        dir_a, dir_b,
        "different lib sources must not collide in the cache"
    );
}

#[test]
fn lib_static_state_does_not_leak_between_box_runs() {
    // Two tests can share one cached lib classpath (that is the point of the cache), so a box() that
    // mutates a lib `object`'s static state must NOT poison the next box() against the same lib:
    // directory-classpath classes are loaded per-request, not once per runner JVM.
    let lib = "package cachedemo\nobject Counter {\n  var slot: String = \"INIT\"\n}\n";
    let fixture = common::Fixture::new().lib("Lib.kt", lib);
    let first = fixture.run_box(
        "import cachedemo.Counter\nfun box(): String {\n  Counter.slot = \"DIRTY\"\n  return Counter.slot\n}\n",
    );
    assert_eq!(first, "DIRTY");
    let second = fixture.run_box("import cachedemo.Counter\nfun box(): String = Counter.slot\n");
    assert_eq!(
        second, "INIT",
        "lib static state leaked across box() runs sharing a cached classpath"
    );
}

#[test]
fn cached_lib_still_runs_box_against() {
    let lib = "package cachedemo\nfun cachedValue(): String = \"OK\"\n";
    let fixture = common::Fixture::new().lib("Lib.kt", lib);
    // Twice: second call must hit the cache and still produce a runnable classpath.
    for _ in 0..2 {
        fixture.assert_box_ok("import cachedemo.cachedValue\nfun box(): String = cachedValue()\n");
    }
}
