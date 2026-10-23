//! A `super` call maps each argument written for the selected declaration's `vararg` parameter to
//! one element, as an ordinary call does. Room's generated database implementation clears its
//! tables through `super.performClear(true, "a", "b")`.

use super::common;

#[test]
fn super_call_vararg_elements_match_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Clear.kt",
        "abstract class Store {\n\
         \x20   protected fun performClear(foreignKeys: Boolean, vararg tables: String): Int =\n\
         \x20       if (foreignKeys) tables.size else -tables.size\n\
         }\n\
         \n\
         interface Counter {\n\
         \x20   fun count(vararg values: Int): Int = values.sum()\n\
         }\n\
         \n\
         class Tables : Store(), Counter {\n\
         \x20   fun clearAll(): Int = super.performClear(true, \"news\", \"topics\", \"queries\")\n\
         \x20   fun clearOne(): Int = super.performClear(false, \"news\")\n\
         \x20   fun clearNone(): Int = super.performClear(true)\n\
         \x20   fun clearSpread(extra: Array<String>): Int = super.performClear(true, *extra)\n\
         \x20   override fun count(vararg values: Int): Int = super.count(1, 2, *values)\n\
         }\n",
    )]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
