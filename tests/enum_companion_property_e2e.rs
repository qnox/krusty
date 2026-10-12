//! An enum class's companion properties are stored like any class companion's: private statics of
//! the enum, reached through `access$get…$cp`/`access$set…$cp`. The companion's initializer runs in
//! the enum's `<clinit>` after the entries, `$VALUES`, `$ENTRIES` and the `Companion` store, in
//! source order with its `init` blocks. A companion `const val` is a `ConstantValue` field of the
//! enum, ordered among the hoisted fields by declaration.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
var trail = \"\"\n\
\n\
enum class Level(val code: Int) {\n\
    LOW(1),\n\
    HIGH(2);\n\
\n\
    init {\n\
        trail += code\n\
    }\n\
\n\
    companion object {\n\
        init {\n\
            trail += \"a\"\n\
        }\n\
        val first = values()[0]\n\
        val label: String\n\
        var count = 3\n\
\n\
        init {\n\
            label = \"L\" + first.code\n\
        }\n\
\n\
        fun describe() = label + count\n\
    }\n\
}\n\
\n\
enum class Plain {\n\
    ONE;\n\
\n\
    companion object {\n\
        val size = 1\n\
        const val MAX = 9\n\
        var note: String? = null\n\
        var zero = 0\n\
    }\n\
}\n\
";

#[test]
fn an_enum_companion_property_is_an_enum_static() {
    for class in [
        "store/Level",
        "store/Level$Companion",
        "store/Plain",
        "store/Plain$Companion",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "EnumCompanionProperty",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

#[test]
fn enum_companion_fields_do_not_shadow_synthetic_storage() {
    const COLLISIONS: &str = r#"
enum class E {
    A;

    companion object {
        val `$VALUES`: Array<E> = arrayOf(E.A)
        val `$ENTRIES`: kotlin.enums.EnumEntries<E> = E.entries
        val A: E = E.A
        val Companion: E.Companion = this
    }
}

fun box(): String {
    val companion = E.Companion
    return if (
        E.`$VALUES`[0] == E.A &&
        E.`$ENTRIES`.first() == E.A &&
        companion.A == E.A &&
        companion.Companion === companion
    ) "OK" else "fail"
}
"#;

    for class in ["E", "E$Companion"] {
        common::byte_diff_against_kotlinc_cp(
            "EnumCompanionSyntheticFieldCollision",
            COLLISIONS,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
    common::expect_box_same_as_kotlinc(COLLISIONS, "EnumCompanionSyntheticFieldCollisionRun");
}

/// Ordinary companion storage remains private implementation state: unlike a public `@JvmField`,
/// it receives `$N` when its source name matches an enum entry even if the descriptors differ.
#[test]
fn an_ordinary_enum_companion_field_yields_its_name_to_an_entry() {
    const SOURCE: &str = r#"
enum class E {
    A;

    companion object {
        val A: String = "x"
    }
}

fun box(): String = if (E.Companion.A == "x") "OK" else "fail"
"#;

    for class in ["E", "E$Companion"] {
        common::byte_diff_against_kotlinc_cp(
            "EnumCompanionDistinctDescriptorCollision",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
    common::expect_box_same_as_kotlinc(SOURCE, "EnumCompanionDistinctDescriptorCollisionRun");
}
