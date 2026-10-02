//! The zero-argument `kotlin.enums.enumEntries<E>()` has a throwing stdlib body. kotlinc replaces
//! it with `E.getEntries()`, including when a reified inline function forwards its type parameter.
//! A same-named local declaration, including a one-argument overload, stays an ordinary call.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const LISTED: &str = "\
import kotlin.enums.enumEntries\n\
\n\
enum class Color { RED, GREEN }\n\
\n\
@OptIn(ExperimentalStdlibApi::class)\n\
fun listed(): String = enumEntries<Color>().toString()\n\
\n\
fun box(): String = if (listed() == \"[RED, GREEN]\") \"OK\" else listed()\n";

#[test]
fn enum_entries_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(LISTED, "EnumEntriesIntrinsic");
}

#[test]
fn enum_entries_emits_get_entries() {
    let Some(built) = compare_with_kotlinc_plugin(
        "EnumEntriesIntrinsic",
        LISTED,
        "EnumEntriesIntrinsicKt",
        &[common::stdlib_jar()],
        "21",
        &[],
    ) else {
        panic!("reference kotlinc or javap unavailable");
    };
    let krusty = method_instructions(&built.krusty, "String listed();");
    let reference = method_instructions(&built.reference, "String listed();");
    assert!(
        krusty
            .iter()
            .any(|line| line.contains("Color.getEntries:()Lkotlin/enums/EnumEntries;")),
        "listed() did not call Color.getEntries: {krusty:?}"
    );
    assert_eq!(krusty, reference);
}

#[test]
fn enum_entries_forwarded_through_reified_inline() {
    let src = "\
import kotlin.enums.enumEntries\n\
\n\
enum class Color { RED, GREEN }\n\
\n\
@OptIn(ExperimentalStdlibApi::class)\n\
inline fun <reified T : Enum<T>> listed(): String = enumEntries<T>().toString()\n\
\n\
fun box(): String = if (listed<Color>() == \"[RED, GREEN]\") \"OK\" else listed<Color>()\n";
    common::expect_box_same_as_kotlinc(src, "EnumEntriesReified");
}

#[test]
fn a_same_named_local_enum_entries_stays_ordinary() {
    let src = "\
enum class Color { RED, GREEN }\n\
\n\
fun <T : Enum<T>> enumEntries(): String = \"local\"\n\
\n\
fun box(): String {\n\
    val listed = enumEntries<Color>()\n\
    return if (listed == \"local\") \"OK\" else listed\n\
}\n";
    common::expect_box_same_as_kotlinc(src, "EnumEntriesNameClash");
    let Some(built) = compare_with_kotlinc_plugin(
        "EnumEntriesNameClash",
        src,
        "EnumEntriesNameClashKt",
        &[common::stdlib_jar()],
        "21",
        &[],
    ) else {
        panic!("reference kotlinc or javap unavailable");
    };
    let krusty = method_instructions(&built.krusty, "String box();");
    let reference = method_instructions(&built.reference, "String box();");
    assert!(
        krusty
            .iter()
            .all(|line| !line.contains("getEntries:()Lkotlin/enums/EnumEntries;")),
        "a local enumEntries was rewritten to getEntries: {krusty:?}"
    );
    assert_eq!(krusty, reference);
}

#[test]
fn an_enum_entry_named_entries_does_not_hide_enum_entries() {
    let src = "\
// LANGUAGE: -PrioritizedEnumEntries -ForbidEnumEntryNamedEntries\n\
import kotlin.enums.enumEntries\n\
\n\
enum class EnumWithClash {\n\
    values, entries, valueOf;\n\
}\n\
\n\
@OptIn(ExperimentalStdlibApi::class)\n\
fun box(): String {\n\
    if (enumEntries<EnumWithClash>().toString() != \"[values, entries, valueOf]\") return \"fail intrinsic\"\n\
    if (EnumWithClash.entries.toString() != \"entries\") return \"fail entry\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_same_as_kotlinc(src, "EnumEntriesEntryClash");
}

#[test]
fn a_same_named_one_argument_function_stays_ordinary() {
    let src = "\
enum class Color { RED, GREEN }\n\
\n\
fun <T : Enum<T>> enumEntries(values: Array<T>): String = \"array:\" + values.size\n\
\n\
fun box(): String {\n\
    val listed = enumEntries(arrayOf(Color.RED, Color.GREEN))\n\
    return if (listed == \"array:2\") \"OK\" else listed\n\
}\n";
    common::expect_box_same_as_kotlinc(src, "EnumEntriesArrayName");
    let Some(built) = compare_with_kotlinc_plugin(
        "EnumEntriesArrayName",
        src,
        "EnumEntriesArrayNameKt",
        &[common::stdlib_jar()],
        "21",
        &[],
    ) else {
        panic!("reference kotlinc or javap unavailable");
    };
    let krusty = method_instructions(&built.krusty, "String box();");
    let reference = method_instructions(&built.reference, "String box();");
    assert!(
        krusty
            .iter()
            .all(|line| !line.contains("getEntries:()Lkotlin/enums/EnumEntries;")),
        "a one-argument enumEntries was rewritten to getEntries: {krusty:?}"
    );
    assert_eq!(krusty, reference);
}
