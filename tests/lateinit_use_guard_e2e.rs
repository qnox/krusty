//! A `lateinit` read keeps the value on the stack and throws while it is null. kotlinc's guard is
//! `dup; ifnonnull L; pop; ldc "x"; invokestatic throwUninitializedPropertyAccessException;
//! aconst_null; L:` at every use site: a member field read, a read through `access$get…$p`, a named
//! object's static field, and a local variable.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Slot {\n\
    private lateinit var item: String\n\
    fun read(): String = item\n\
    fun check(): Boolean {\n\
        try {\n\
            val seen = item\n\
            return false\n\
        } catch (e: RuntimeException) {\n\
            return true\n\
        }\n\
    }\n\
    class Peek {\n\
        fun look(slot: Slot): String = slot.item\n\
    }\n\
}\n\
\n\
object Shelf {\n\
    private lateinit var top: String\n\
    fun read(): String = top\n\
}\n\
\n\
fun later(flag: Boolean): String {\n\
    lateinit var note: String\n\
    if (flag) note = \"OK\"\n\
    return note\n\
}\n\
";

#[test]
fn a_lateinit_read_guards_like_kotlinc() {
    for class in [
        "store/Slot",
        "store/Slot$Peek",
        "store/Shelf",
        "store/LateinitUseGuardKt",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "LateinitUseGuard",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
