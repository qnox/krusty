//! Constructing another file's class through a constructor that takes a value class.
//!
//! kotlinc makes such a constructor private and publishes a synthetic accessor with a trailing
//! `DefaultConstructorMarker`; every construction from outside the class goes through the accessor.
//! krusty recognized the constructor only when the constructing code shared the class's file — the
//! test read the target class's own declaration. From any other file the call named the private
//! primary and failed at run time:
//!
//! ```text
//! IllegalAccessError: class MainKt tried to access private method 'void Holder.<init>(int)'
//! ```
//!
//! The value-class pass now records such a construction from the node's own parameter types, before
//! it erases them.
use super::common;

const TAG: &str = "@JvmInline\n\
    value class Tag(val value: Int)\n";

fn expect_same_box(sources: &[(&str, &str)], stem: &str) {
    common::expect_box_ok_files_with_stdlib(sources, stem);
    assert_eq!(
        common::kotlinc_box_files_result(sources, "MainKt"),
        "OK",
        "{stem}: kotlinc reference"
    );
}

#[test]
fn a_primary_constructor_is_reached_through_its_accessor() {
    const HOLDER: &str = "class Holder(val count: Tag)\n";
    const MAIN: &str = "fun box(): String {\n\
        \x20   val holder = Holder(Tag(3))\n\
        \x20   return if (holder.count.value == 3) \"OK\" else \"FAIL \" + holder.count.value\n\
        }\n";
    expect_same_box(
        &[("Tag.kt", TAG), ("Holder.kt", HOLDER), ("Main.kt", MAIN)],
        "a sibling primary constructor taking a value class",
    );
}

#[test]
fn a_secondary_constructor_is_reached_through_its_accessor() {
    const HOLDER: &str = "class Holder(val count: Int) {\n\
        \x20   constructor(tag: Tag, extra: Int) : this(tag.value + extra)\n\
        }\n";
    const MAIN: &str = "fun box(): String {\n\
        \x20   val holder = Holder(Tag(3), 4)\n\
        \x20   return if (holder.count == 7) \"OK\" else \"FAIL \" + holder.count\n\
        }\n";
    expect_same_box(
        &[("Tag.kt", TAG), ("Holder.kt", HOLDER), ("Main.kt", MAIN)],
        "a sibling secondary constructor taking a value class",
    );
}

/// A type parameter bound to a value class at the call site is not a value-class parameter: the
/// constructor stays public and takes the box.
#[test]
fn a_generic_constructor_instantiated_with_a_value_class_stays_direct() {
    const HOLDER: &str = "class Holder<T>(val item: T)\n";
    const MAIN: &str = "fun box(): String {\n\
        \x20   val holder = Holder(Tag(3))\n\
        \x20   return if (holder.item.value == 3) \"OK\" else \"FAIL \" + holder.item.value\n\
        }\n";
    expect_same_box(
        &[("Tag.kt", TAG), ("Holder.kt", HOLDER), ("Main.kt", MAIN)],
        "a sibling generic constructor instantiated with a value class",
    );
}

/// Source-distinct constructors that erase to the same JVM `<init>` are invalid. The backend owns
/// that representation fact, but it must diagnose the clash before emitting an unloadable class.
#[test]
fn same_erasure_secondary_overloads_are_rejected_before_emission() {
    const MAIN: &str = "@JvmInline\n\
        value class Tag(val value: Int)\n\
        class Holder {\n\
        \x20   val count: Int\n\
        \x20   constructor(count: Int) { this.count = count }\n\
        \x20   constructor(tag: Tag) { this.count = tag.value + 10 }\n\
        }\n\
        fun box(): String {\n\
        \x20   val plain = Holder(3)\n\
        \x20   val tagged = Holder(Tag(4))\n\
        \x20   return if (plain.count == 3 && tagged.count == 14) \"OK\" else \"FAIL\"\n\
        }\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::backend_outcome_in_process(MAIN, "Main", &[stdlib], Some(jdk.as_path())),
        Some(common::BackendOutcome::Rejected(vec![
            "platform declaration clash: 'Holder' contains duplicate JVM constructor <init>(I)V"
                .to_string(),
        ])),
        "the complete backend diagnostic set",
    );
    let (reference_code, _) = common::kotlinc_source_result("SameErasureSecondary", MAIN);
    assert_eq!(reference_code, 1, "kotlinc must reject the same JVM clash");
}
