//! A not-null operator (`!!`) applied to a nullable intersection fixed below an upper type. kotlinc
//! releases with `Quirk::NotNullOperatorDropsIntersectionApproximation` narrow it to a new
//! intersection that publishes `Any`; later ones keep the upper type, so the declaration publishes
//! the bound. A guard (`if (x != null) x`) publishes `Any` in every release.
//!
//! The program is the shared box regression, whose `box()` asserts only what every release agrees
//! on. This test runs it with every input and assertion kept and also returns its `describe()`,
//! pinning which answer the reference release gives for `!!`.

use krusty::kotlin_version::{Quirk, ReferenceProfile};

use super::common;

const PORTABLE: &str = include_str!("box_regressions/nonNullIntersectionPublishesAny.kt");

/// The shared program with its `box()` renamed `portableBox()`, and a `box()` that answers
/// `portableBox()`'s result followed by `describe()`.
fn source() -> String {
    let portable = "fun box(): String {";
    assert_eq!(
        PORTABLE.matches(portable).count(),
        1,
        "one box() in the fixture"
    );
    let renamed = PORTABLE.replacen(portable, "fun portableBox(): String {", 1);
    format!("{renamed}\nfun box(): String = portableBox() + \";\" + describe()\n")
}

#[test]
fn a_not_null_assertion_publishes_the_type_the_reference_release_does() {
    let drops = ReferenceProfile::of_release(krusty::kotlin_version::environment_release())
        .has(Quirk::NotNullOperatorDropsIntersectionApproximation);
    let source = source();
    let reference = common::kotlinc_box_result(&source);
    assert_eq!(
        reference,
        if drops { "OK;any" } else { "OK;base" },
        "kotlinc"
    );
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty = common::compile_and_run_box(
        &source,
        "Main",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
    assert_eq!(krusty.as_deref(), Some(reference.as_str()), "krusty");
}
