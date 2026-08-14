//! Harness semantics for dependency libs (`compile_libs` / `Fixture`):
//!
//! - Libs are compiled BY KRUSTY, in-process; a lib krusty can't build FAILS the test with
//!   krusty's diagnostics — no reference-compiler fallback, no silent skip.
//! - Builds are memoized per run (same sources → same classpath dir), which both dedupes compiles
//!   and keeps the box-runner JVM shared across tests using the same lib.
//! - Sharing a classpath dir must NOT share JVM static state between `box()` runs.
//!
//! These invariants carry the e2e suite's wall clock and its correctness boundary — keep them.

use super::common;

fn expect_lib(tag: &str, src: &str) -> std::path::PathBuf {
    common::compile_lib(tag, src)
        .unwrap_or_else(|| panic!("{tag}: scratch filesystem unavailable for dependency lib"))
}

#[test]
fn same_lib_source_reuses_memoized_classpath_dir() {
    let src = "package cachedemo\nfun seed(): Int = 41\n";
    let first = expect_lib("libmemo_same", src);
    let second = expect_lib("libmemo_same", src);
    assert_eq!(
        first, second,
        "identical lib sources must map to one memoized classpath dir"
    );
    assert!(
        std::fs::read_dir(&first)
            .map(|d| d.count() > 0)
            .unwrap_or(false),
        "lib classpath dir must contain compiled output"
    );
}

#[test]
fn different_lib_source_gets_distinct_classpath_dir() {
    let dir_a = expect_lib(
        "libmemo_diff_a",
        "package cachedemo\nfun seedA(): Int = 1\n",
    );
    let dir_b = expect_lib(
        "libmemo_diff_b",
        "package cachedemo\nfun seedB(): Int = 2\n",
    );
    assert_ne!(
        dir_a, dir_b,
        "different lib sources must not collide in the memo"
    );
}

#[test]
fn lib_is_krusty_built() {
    // The dependency compiler IS krusty: the build must exist without any reference kotlinc
    // involvement, and carry the module facade index krusty's own consumer needs.
    let build = common::compile_libs_build(
        "libmemo_krusty",
        &[(
            "Lib.kt",
            "package cachedemo\nfun krustyBuilt(): String = \"yes\"\n",
        )],
    )
    .expect("scratch filesystem");
    let out = build.krusty_out();
    assert!(
        std::fs::read_dir(out)
            .map(|d| d.count() > 0)
            .unwrap_or(false),
        "krusty-built lib dir is empty"
    );
}

#[test]
#[should_panic(expected = "krusty failed to compile")]
fn unbuildable_lib_fails_with_diagnostics() {
    // Invalid Kotlin must FAIL the test with krusty's diagnostics — never skip, never fall back.
    let _ = common::compile_lib(
        "libmemo_invalid",
        "package cachedemo\nfun broken(): NoSuchType = 1\n",
    );
}

#[test]
fn lib_static_state_does_not_leak_between_box_runs() {
    // Tests share one memoized lib classpath, so a box() that mutates a lib `object`'s static
    // state must NOT poison the next box() against the same lib: directory-classpath classes are
    // loaded per-request, not once per runner JVM.
    let lib = "package cachedemo\nobject Counter {\n  var slot: String = \"INIT\"\n}\n";
    let fixture = common::Fixture::new().lib("Lib.kt", lib);
    let first = fixture.run_box(
        "import cachedemo.Counter\nfun box(): String {\n  Counter.slot = \"DIRTY\"\n  return Counter.slot\n}\n",
    );
    assert_eq!(first, "DIRTY");
    let second = fixture.run_box("import cachedemo.Counter\nfun box(): String = Counter.slot\n");
    assert_eq!(
        second, "INIT",
        "lib static state leaked across box() runs sharing a lib classpath"
    );
}

#[test]
fn krusty_built_lib_serves_box_repeatedly() {
    let lib = "package cachedemo\nfun cachedValue(): String = \"OK\"\n";
    let fixture = common::Fixture::new().lib("Lib.kt", lib);
    // Twice: second call must hit the memo and still produce a runnable classpath.
    for _ in 0..2 {
        fixture.assert_box_ok("import cachedemo.cachedValue\nfun box(): String = cachedValue()\n");
    }
}
