//! The zero-argument `kotlin.enums.enumEntries<E>()` has a throwing stdlib body. kotlinc replaces
//! it with `E.getEntries()`. The one-argument overloads stay ordinary calls.
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
fn enum_entries_array_overload_stays_ordinary() {
    let src = "\
import kotlin.enums.enumEntries\n\
\n\
enum class Color { RED, GREEN }\n\
\n\
@OptIn(ExperimentalStdlibApi::class)\n\
fun box(): String {\n\
    val listed = enumEntries(arrayOf(Color.RED, Color.GREEN))\n\
    return if (listed.toString() == \"[RED, GREEN]\") \"OK\" else listed.toString()\n\
}\n";
    common::expect_box_same_as_kotlinc(src, "EnumEntriesArrayOverload");
}
