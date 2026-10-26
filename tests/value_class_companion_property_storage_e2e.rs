//! Where a value class's companion properties are stored, against kotlinc.
//!
//! kotlinc stores a value class's companion property as it does an ordinary class's: a private
//! static field of the value class itself, initialized in its `<clinit>` after the `Companion`
//! instance, which the companion's accessors reach through `access$getX$cp`/`access$setX$cp`. krusty
//! kept every such field as an instance field of `Companion`, initialized by its constructor.

use super::common;

const SOURCE: &str = r#"fun seed() = 3

@JvmInline
value class Text(val v: String) {
    companion object {
        val n = seed()
        var m = "x"
        private val p = 4
        val make = { s: String -> Text(s) }

        fun usesP() = p
    }
}

fun box(): String {
    Text.m = Text.m + "y"
    val ok = Text.n == 3 && Text.m == "xy" && Text.usesP() == 4
    return if (ok) Text.make("OK").v else "fail"
}
"#;

#[test]
fn companion_properties_are_statics_of_the_value_class() {
    common::assert_classes_identical_to_kotlinc(
        "ValueClassCompanionStorage",
        SOURCE,
        &["Text", "Text$Companion"],
    );
}

#[test]
fn companion_properties_run() {
    common::expect_box_ok_with_stdlib(SOURCE, "ValueClassCompanionStorage");
}

/// An `init` block moves the whole companion initializer to the value class's `<clinit>`, in
/// source order, over every kind of stored companion property.
const WITH_INIT: &str = r#"@JvmInline
value class Id(val raw: Int) {
    companion object {
        const val C = 7
        val zero = Id(0)
        val maybe: Id? = Id(1)
        @JvmField val jf = "j"
        lateinit var late: String
        var counter = 0
        init {
            counter += 2
            late = "L"
        }
        val after = counter + 1
    }
}

fun box(): String {
    val ok = Id.zero.raw == 0 && Id.maybe?.raw == 1 && Id.jf == "j" && Id.late == "L" &&
        Id.counter == 2 && Id.after == 3 && Id.C == 7
    return if (ok) "OK" else "fail"
}
"#;

#[test]
fn companion_initializer_runs_in_the_value_class_clinit() {
    common::assert_classes_identical_to_kotlinc("ValueClassCompanionInit", WITH_INIT, &["Id"]);
}

#[test]
fn companion_initializer_runs() {
    common::expect_box_ok_with_stdlib(WITH_INIT, "ValueClassCompanionInit");
}
