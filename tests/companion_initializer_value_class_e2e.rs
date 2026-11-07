//! A companion initializer that the outer class's `<clinit>` runs constructs value classes through
//! their `constructor-impl`, against kotlinc.
//!
//! With an `init` block the whole companion initializer moves to the outer `<clinit>`, keeping its
//! source order. The value-class rewrite visited the companion's initializer only while the
//! companion still owned it, so the moved body kept `new Id(0)` and stored the box where the field
//! holds the carrier: a `VerifyError`.

use super::common;

const SOURCE: &str = r#"@JvmInline
value class Id(val raw: Int)

class Holder {
    companion object {
        var counter = 0
        val zero = Id(0)
        init {
            counter += 1
        }
    }
}

fun box(): String = if (Holder.zero.raw == 0 && Holder.counter == 1) "OK" else "fail"
"#;

#[test]
fn moved_initializer_constructs_the_carrier() {
    common::assert_classes_identical_to_kotlinc(
        "CompanionInitializerValueClass",
        SOURCE,
        &["Holder"],
    );
}

#[test]
fn moved_initializer_runs() {
    common::expect_box_ok_with_stdlib(SOURCE, "CompanionInitializerValueClass");
}
