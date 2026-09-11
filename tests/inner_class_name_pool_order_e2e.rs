//! A nested class whose `InnerClasses` table has a SIBLING row sorting before its own interned the
//! two simple names in the wrong order, leaving the class byte-different from kotlinc while matching
//! it in every other respect.
//!
//! kotlinc interns an entry's outer-class ref and simple name as it VISITS the table, and it visits
//! the table sorted by inner internal name. krusty seeded only the class's OWN row at that point and
//! let `finish` intern the rest, so its own name landed first. `Outer$Alpha` sorts ahead of
//! `Outer$Companion` (`'$'` and `'A'` both precede `'C'`), and the two constants came out
//! transposed.
//!
//! It takes a table with two retained rows to see, which is why single-nested-class fixtures missed
//! it: with one row the two orders coincide. In a serialization-heavy program it is the common
//! shape — a `@Serializable` class's companion always sees the `$serializer` row too, and
//! `Foo$$serializer` sorts ahead of `Foo$Companion`.
use super::common;

/// `Outer$Companion` references `Outer$Alpha`, so its `InnerClasses` table keeps both rows and the
/// sorted order puts `Alpha` first — the case where seeding this class's own name first is wrong.
const SRC: &str = "class Outer {\n\
                   \x20   class Alpha\n\
                   \x20\n\
                   \x20   companion object {\n\
                   \x20       fun make(): Alpha = Alpha()\n\
                   \x20   }\n\
                   }\n";

/// A table spanning a NESTING CHAIN — `A$B`, `A$B$Alpha`, `A$B$Companion`, whose rows carry two
/// different outers. Two things have to hold here that a flat table cannot show: every row's class
/// entry is interned before ANY simple name (interleaving per row transposes them once the outers
/// differ), and the retained set has to be a FIXPOINT, because interning one row's outer ref is
/// what makes the enclosing row referenced and therefore kept.
#[test]
fn a_nesting_chain_interns_every_class_entry_before_any_name() {
    let src = "class Outer {\n\
               \x20   class Middle {\n\
               \x20       class Alpha\n\
               \x20\n\
               \x20       companion object {\n\
               \x20           fun make(): Alpha = Alpha()\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    for class in ["Outer$Middle$Companion", "Outer$Middle$Alpha"] {
        let Some(result) = common::byte_diff_against_kotlinc("InnerNameChain", src, class) else {
            eprintln!("skipping: reference kotlinc unavailable");
            return;
        };
        result.unwrap_or_else(|e| panic!("{class} byte-identical to kotlinc: {e}"));
    }
}

#[test]
fn companion_interns_a_sibling_inner_name_first() {
    let Some(result) = common::byte_diff_against_kotlinc("InnerNameOrder", SRC, "Outer$Companion")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Outer$Companion byte-identical to kotlinc");
}

/// The sibling itself has a single-row table, so its order was never wrong — keep it executable so a
/// change that reorders or drops rows is caught rather than silently trading one class for another.
#[test]
fn a_single_row_table_is_unchanged() {
    let Some(result) = common::byte_diff_against_kotlinc("InnerNameOrderAlpha", SRC, "Outer$Alpha")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Outer$Alpha byte-identical to kotlinc");
}
