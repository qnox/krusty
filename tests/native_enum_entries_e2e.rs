//! `E.entries` and `enumEntries<E>()`: an enum's constants as a read-only list.
//!
//! Kotlin/Native answers both with one `kotlin.enums.EnumEntriesList` per enum class, built over
//! the constants in declaration order the first time it is asked for. So the list is the SAME
//! object on every read, compares and renders like any other list, and names its own class.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// What a list is asked: its size, an element by index, a walk, and a search.
#[test]
fn entries_answer_as_a_list_of_the_constants_in_declaration_order() {
    let source = "enum class Colour { RED, GREEN, BLUE }\n\
         fun box(): String {\n\
         \x20   val entries = Colour.entries\n\
         \x20   if (entries.size != 3) return \"fail size \" + entries.size\n\
         \x20   if (entries[1] != Colour.GREEN) return \"fail get\"\n\
         \x20   var names = \"\"\n\
         \x20   for (colour in entries) names += colour.name\n\
         \x20   if (names != \"REDGREENBLUE\") return \"fail walk \" + names\n\
         \x20   if (entries.indexOf(Colour.BLUE) != 2) return \"fail indexOf\"\n\
         \x20   if (!entries.contains(Colour.RED)) return \"fail contains\"\n\
         \x20   if (entries.toString() != \"[RED, GREEN, BLUE]\") return \"fail \" + entries\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("EnumEntriesAsList", source);
}

/// One list per enum: every read hands out the same object, and it equals a list of the same
/// constants built any other way.
#[test]
fn entries_are_one_list_per_enum_and_equal_to_any_list_of_the_same_constants() {
    let source = "enum class Step { ONE, TWO }\n\
         fun box(): String {\n\
         \x20   if (Step.entries !== Step.entries) return \"fail identity\"\n\
         \x20   if (Step.entries != listOf(Step.ONE, Step.TWO)) return \"fail equals\"\n\
         \x20   if (listOf(Step.ONE, Step.TWO) != Step.entries) return \"fail equals back\"\n\
         \x20   if (Step.entries.hashCode() != listOf(Step.ONE, Step.TWO).hashCode()) return \"fail hash\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("EnumEntriesIdentity", source);
}

/// The list is Kotlin's own enum-entries class, whatever spelling asked for it.
#[test]
fn entries_name_the_enum_entries_list_class() {
    let source = "enum class Mode { ON, OFF }\n\
         fun box(): String {\n\
         \x20   val name = Mode.entries::class.simpleName\n\
         \x20   return if (name == \"EnumEntriesList\") \"OK\" else \"fail \" + name\n\
         }\n";
    every_backend_agrees_with_kotlinc("EnumEntriesClassName", source);
}

/// Walking the list uses `AbstractList`'s own iterator, which is what kotlinc-native 2.4.20 names
/// (`IteratorImpl kotlin.collections.AbstractList.IteratorImpl`). The JVM's iterator is a
/// different class, so this one is Native's alone.
#[test]
fn entries_are_walked_by_the_abstract_list_iterator() {
    let source = "enum class Mode { ON, OFF }\n\
         fun box(): String {\n\
         \x20   val walk = Mode.entries.iterator()\n\
         \x20   val name = walk::class.qualifiedName\n\
         \x20   if (name != \"kotlin.collections.AbstractList.IteratorImpl\") return \"fail \" + name\n\
         \x20   return if (walk.next() == Mode.ON) \"OK\" else \"fail next\"\n\
         }\n";
    expect_native_box(source, "EnumEntriesIterator", "OK");
}

/// `enumEntries<E>()` is the same list `E.entries` is.
#[test]
fn enum_entries_of_a_type_is_that_enums_entries() {
    let source = "import kotlin.enums.enumEntries\n\
         enum class Size { S, M, L }\n\
         fun box(): String {\n\
         \x20   val listed = enumEntries<Size>()\n\
         \x20   if (listed !== Size.entries) return \"fail identity\"\n\
         \x20   return if (listed.last() == Size.L) \"OK\" else \"fail last\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("EnumEntriesOfType", source);
}

/// An enum with no constants has an empty list, not a missing one.
#[test]
fn an_enum_without_constants_has_empty_entries() {
    let source = "enum class Nothing2\n\
         fun box(): String {\n\
         \x20   if (!Nothing2.entries.isEmpty()) return \"fail empty\"\n\
         \x20   return if (Nothing2.entries.toString() == \"[]\") \"OK\" else \"fail render\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("EnumEntriesEmpty", source);
}

/// `E.entries` is an `EnumEntries` at run time: an `is`, a safe cast and a cast all see it, and a
/// list that is not one is told apart.
#[test]
fn entries_are_an_enum_entries_at_run_time() {
    let source = "import kotlin.enums.EnumEntries\n\
         enum class Mode { ON, OFF }\n\
         fun box(): String {\n\
         \x20   val entries: Any = Mode.entries\n\
         \x20   val list: Any = listOf(Mode.ON, Mode.OFF)\n\
         \x20   if (entries !is EnumEntries<*>) return \"fail is\"\n\
         \x20   if (list is EnumEntries<*>) return \"fail list is\"\n\
         \x20   if ((entries as? EnumEntries<*>)?.size != 2) return \"fail safe cast\"\n\
         \x20   if ((list as? EnumEntries<*>) != null) return \"fail list safe cast\"\n\
         \x20   if ((entries as EnumEntries<*>)[1] != Mode.OFF) return \"fail cast\"\n\
         \x20   try {\n\
         \x20       list as EnumEntries<*>\n\
         \x20       return \"fail list cast\"\n\
         \x20   } catch (e: ClassCastException) {}\n\
         \x20   if (entries !is List<*>) return \"fail list interface\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("EnumEntriesRuntimeType", source);
}

/// `$ENTRIES` is assigned after the last constant and before the companion: a constant's own
/// construction reads `null`, and the companion reads every constant. kotlinc-native 2.4.20 prints
/// `A:null;B:null;companion:[A, B];` for this program.
#[test]
fn entries_are_made_after_the_constants_and_before_the_companion() {
    let source = "var log = \"\"\n\
         enum class E {\n\
         \x20   A, B;\n\
         \x20   init { log += \"$name:${E.entries};\" }\n\
         \x20   companion object { init { log += \"companion:${E.entries};\" } }\n\
         }\n\
         fun box(): String {\n\
         \x20   if (E.entries.size != 2) return \"fail size\"\n\
         \x20   return if (log == \"A:null;B:null;companion:[A, B];\") \"OK\" else \"fail \" + log\n\
         }\n";
    expect_native_box(source, "EnumEntriesInitOrder", "OK");
}

/// A constant whose construction throws leaves no entries behind: every read throws again, and
/// none hands out a list over the constants that did get made.
#[test]
fn a_throwing_constant_leaves_no_entries_behind() {
    let source = "enum class Bad { X, Y; init { if (name == \"Y\") throw IllegalStateException(\"boom\") } }\n\
         fun box(): String {\n\
         \x20   for (attempt in 0..1) {\n\
         \x20       val seen = try { \"size \" + Bad.entries.size } catch (e: Throwable) { \"threw\" }\n\
         \x20       if (seen != \"threw\") return \"fail $attempt: $seen\"\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("EnumEntriesThrowingConstant", source);
}

/// A companion that throws comes after `$ENTRIES` is made, and still leaves the enum unbuilt: the
/// list made before it is withdrawn with the constants.
#[test]
fn a_throwing_companion_withdraws_the_entries() {
    let source = "enum class Bad {\n\
         \x20   X;\n\
         \x20   companion object { init { throw IllegalStateException(\"boom\") } }\n\
         }\n\
         fun box(): String {\n\
         \x20   for (attempt in 0..1) {\n\
         \x20       val seen = try { \"size \" + Bad.entries.size } catch (e: Throwable) { \"threw\" }\n\
         \x20       if (seen != \"threw\") return \"fail $attempt: $seen\"\n\
         \x20   }\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("EnumEntriesThrowingCompanion", source);
}

/// After a failed initialization, krusty builds the enum again on the next access (Kotlin/Native
/// and the JVM instead fail every later access). When that rebuild succeeds, `entries` holds the
/// constants of the rebuild, never those of the attempt that failed.
#[test]
fn entries_after_a_retried_initialization_hold_the_rebuilt_constants() {
    let source = "var attempts = 0\n\
         enum class Flaky { X, Y; init { if (name == \"Y\" && attempts++ == 0) throw IllegalStateException(\"boom\") } }\n\
         fun box(): String {\n\
         \x20   try { Flaky.entries; return \"fail first\" } catch (e: IllegalStateException) {}\n\
         \x20   val entries = Flaky.entries\n\
         \x20   if (entries.size != 2) return \"fail size\"\n\
         \x20   if (entries[0] !== Flaky.X || entries[1] !== Flaky.Y) return \"fail stale\"\n\
         \x20   return if (entries === Flaky.entries) \"OK\" else \"fail identity\"\n\
         }\n";
    expect_native_box(source, "EnumEntriesRetry", "OK");
}
