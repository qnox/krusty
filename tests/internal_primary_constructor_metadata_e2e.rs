//! An `internal` primary constructor's `@Metadata` record, against kotlinc.
//!
//! `Constructor.flags` defaults to 6 (a public constructor) and is written only when it differs.
//! An internal constructor's visibility bits are 0, so kotlinc writes an explicit `flags: 0`; with
//! annotations it is 1. krusty omitted the field for an internal primary constructor, publishing
//! it as public to every Kotlin consumer: an ordinary class, a data class and a value class alike.

use super::common;

const SOURCE: &str = r#"annotation class Mark

class Plain internal constructor(val v: Int)

class Annotated @Mark internal constructor(val v: Int)

data class Data internal constructor(val v: Int)

@JvmInline
value class Wrapped internal constructor(val v: Int)

fun box(): String {
    val got = "" + Plain(1).v + Annotated(2).v + Data(3).v + Wrapped(4).v
    return if (got == "1234") "OK" else got
}
"#;

#[test]
fn the_constructor_record_keeps_its_internal_visibility() {
    common::assert_classes_identical_to_kotlinc(
        "InternalPrimaryConstructor",
        SOURCE,
        &["Plain", "Annotated", "Data", "Wrapped"],
    );
}

#[test]
fn internal_constructors_construct_in_their_module() {
    common::expect_box_ok_with_stdlib(SOURCE, "InternalPrimaryConstructor");
}
