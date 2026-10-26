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

/// The debug tables of a companion property's mangled accessors, against kotlinc.
///
/// A companion property typed as a value class is stored as a static of the outer class, and the
/// companion keeps its accessors under their mangled names (`getZero-eEFUqEU`). kotlinc writes them
/// as it writes any accessor: the getter maps the property's line and names `this`, and the setter
/// also names its `<set-?>` parameter. krusty described the getter under its unmangled spelling,
/// which names no method, so the mangled one had no tables, in a value class's companion and an
/// ordinary one's.
const MANGLED_ACCESSORS: &str = r#"@JvmInline
value class Id(val raw: Int) {
    companion object {
        val zero = Id(0)
        var current = Id(1)
    }
}

@JvmInline
value class Name(val text: String)

class Holder {
    companion object {
        val one = Id(1)
        var name = Name("n")
    }
}

fun box(): String {
    Id.current = Id(2)
    Holder.name = Name("m")
    val ok = Id.zero.raw == 0 && Id.current.raw == 2 && Holder.one.raw == 1 &&
        Holder.name.text == "m"
    return if (ok) "OK" else "fail"
}
"#;

#[test]
fn mangled_companion_accessors_carry_debug_tables() {
    common::assert_classes_identical_to_kotlinc(
        "CompanionValueClassAccessorDebug",
        MANGLED_ACCESSORS,
        &["Id$Companion", "Holder$Companion", "Id", "Holder"],
    );
}

#[test]
fn mangled_companion_accessors_run() {
    common::expect_box_ok_with_stdlib(MANGLED_ACCESSORS, "CompanionValueClassAccessorDebug");
}

/// The metadata of a delegated companion property, against kotlinc.
///
/// A companion property's `x$delegate` is stored as a static of the outer class, like the backing
/// field of an ordinary companion property. kotlinc's companion `@Metadata` still names that field
/// in the property's JVM signature (`lazyTwo$delegate:Lkotlin/Lazy;`); krusty wrote no field
/// signature for it, in a value class's companion and an ordinary class's.
const LAZY_DELEGATES: &str = r#"@JvmInline
value class Id(val raw: Int) {
    companion object {
        val label by lazy { "one" }
    }
}

class Holder {
    companion object {
        val lazyTwo by lazy { "two" }
    }
}

fun box(): String = if (Id.label == "one" && Holder.lazyTwo == "two") "OK" else "fail"
"#;

#[test]
fn companion_delegate_field_is_in_the_companion_metadata() {
    common::assert_classes_identical_to_kotlinc(
        "CompanionLazyDelegateMetadata",
        LAZY_DELEGATES,
        &["Id$Companion", "Holder$Companion"],
    );
}

#[test]
fn companion_delegates_run() {
    common::expect_box_ok_with_stdlib(LAZY_DELEGATES, "CompanionLazyDelegateMetadata");
}
