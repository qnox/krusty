//! kotlinc's `RedundantCheckcastsBeforeAastoreMethodTransformer`, applied to krusty's methods: a
//! `checkcast` whose next instruction is an `aastore` goes, and a reified cast goes together with
//! its `reifiedOperationMarker`. The array store checks the element's class itself.
//!
//! krusty wrote `checkcast Word; aastore` for `take(x as Word?, x)` into a `vararg Any?`, and
//! `checkcast Token; aastore` after the non-null check of `y as Token`. The cast targets are
//! declared here, so no built-in type's handling takes part.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "package store\n\
    interface Token\n\
    class Word : Token\n\
    class Mark : Token\n\
    object Store {\n\
    \x20   fun take(vararg xs: Any?): Int = xs.size\n\
    \x20   fun tokens(vararg xs: Token?): Int = xs.size\n\
    }\n\
    fun castArgument(x: Any?): Int = Store.take(x as Word?, x)\n\
    fun castElements(x: Any?, y: Any?): Int = Store.tokens(x as Word?, y as Token)\n\
    inline fun <reified T> reifiedArgument(x: Any?): Int = Store.take(x as T)\n";

#[test]
fn a_cast_before_an_array_store_is_dropped_like_kotlincs() {
    let built = compare_with_kotlinc_plugin(
        "CastsBeforeAastore",
        SOURCE,
        "store/CastsBeforeAastoreKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are required");
    for member in [
        "int castArgument(java.lang.Object)",
        "int castElements(java.lang.Object, java.lang.Object)",
        "int reifiedArgument(java.lang.Object)",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

/// kotlinc's rule changes what runs: `x as Word?` stored into an `Array<Any?>` no longer checks
/// the class of `x`, while the cast before another cast still does.
#[test]
fn a_store_without_its_cast_takes_any_element() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (castArgument(Mark()) != 2) return \"argument\"\n\
             \x20   if (castElements(Word(), Mark()) != 2) return \"elements\"\n\
             \x20   try {{\n\
             \x20       castElements(Mark(), Word())\n\
             \x20       return \"no cast\"\n\
             \x20   }} catch (e: ClassCastException) {{\n\
             \x20   }}\n\
             \x20   return if (reifiedArgument<Word>(Mark()) == 1) \"OK\" else \"reified\"\n\
             }}\n"
        ),
        "CastsBeforeAastoreBox",
    );
}
