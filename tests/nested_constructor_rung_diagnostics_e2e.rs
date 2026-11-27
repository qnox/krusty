//! `C.Name(args)` whose nested constructors and later rungs (companion members, extensions,
//! function-valued properties) all reject the arguments. kotlinc reports the rejected group with
//! the best applicability across the whole tower: a lone group reports its own diagnostic, and
//! equally applicable groups report "none of the following candidates is applicable:".

use super::common;

#[test]
fn a_constructor_and_a_companion_function_rejecting_alike_are_none_applicable() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            r#"class Config {
    class Entry(val key: String)

    companion object {
        fun Entry(index: Int): Entry = Entry("K$index")
        fun Pair(first: Int, second: Int): Int = first + second
    }

    class Pair(val key: String)
}

fun mismatched() = Config.Entry(true)
fun unmapped(): Any = Config.Pair()
"#,
        )],
        &[],
    );
}

#[test]
fn a_companion_extension_and_a_function_property_join_the_constructor_group() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            r#"class Record {
    class Field(val key: String)

    companion object
}

fun Record.Companion.Field(index: Int): String = "field$index"

class Holder {
    class Item(val key: String)

    companion object {
        val Item: (Int) -> String = { "item$it" }
    }
}

class Wide {
    class Item(val key: String)

    companion object {
        val Item: (Long) -> String = { "wide$it" }
    }
}

fun extension(): Any = Record.Field(true)
fun property(): Any = Holder.Item(true)
fun convertedProperty(): Any = Wide.Item(true)
"#,
        )],
        &[],
    );
}

#[test]
fn the_better_applicable_group_reports_its_own_diagnostic() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            r#"class Two {
    class Item(val key: String)

    companion object
}

fun Two.Companion.Item(first: Int, second: Int): Int = first + second

interface Options {
    interface Builder

    companion object
}

fun Options.Companion.Builder(size: Int): Options.Builder = object : Options.Builder {}

fun constructorMismatch(): Any = Two.Item(1)
fun extensionMismatch(): Any = Two.Item("s", "t")
fun onlyExtension(): Any = Options.Builder("s")
"#,
        )],
        &[],
    );
}
