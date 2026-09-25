//! kotlinc's `RedundantCheckcastsBeforeAastoreMethodTransformer`, applied to krusty's methods: a
//! `checkcast` whose next instruction is an `aastore` goes, and a reified cast goes together with
//! its `reifiedOperationMarker`. The array store checks the element's class itself.
//!
//! krusty wrote `checkcast String; aastore` for `take(x as String?, x)` into a `vararg Any?`, and
//! `checkcast CharSequence; aastore` after the non-null check of `y as CharSequence`.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "object Store {\n\
    \x20   fun take(vararg xs: Any?): Int = xs.size\n\
    \x20   fun chars(vararg xs: CharSequence?): Int = xs.size\n\
    }\n\
    fun castArgument(x: Any?): Int = Store.take(x as String?, x)\n\
    fun castElements(x: Any?, y: Any?): Int = Store.chars(x as String?, y as CharSequence)\n\
    inline fun <reified T> reifiedArgument(x: Any?): Int = Store.take(x as T)\n";

#[test]
fn a_cast_before_an_array_store_is_dropped_like_kotlincs() {
    let built = compare_with_kotlinc_plugin(
        "CastsBeforeAastore",
        SOURCE,
        "CastsBeforeAastoreKt",
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

/// kotlinc's rule changes what runs: `x as String?` stored into an `Array<Any?>` no longer checks
/// the class of `x`, while the cast before another cast still does.
#[test]
fn a_store_without_its_cast_takes_any_element() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (castArgument(1) != 2) return \"argument\"\n\
             \x20   if (castElements(\"a\", \"b\") != 2) return \"elements\"\n\
             \x20   try {{\n\
             \x20       castElements(1, \"b\")\n\
             \x20       return \"no cast\"\n\
             \x20   }} catch (e: ClassCastException) {{\n\
             \x20   }}\n\
             \x20   return if (reifiedArgument<String>(3) == 1) \"OK\" else \"reified\"\n\
             }}\n"
        ),
        "CastsBeforeAastoreBox",
    );
}
