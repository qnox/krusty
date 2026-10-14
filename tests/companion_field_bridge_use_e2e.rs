//! The outer class declares `access$get…$cp` / `access$set…$cp` for a private companion property
//! only when another class uses its hoisted field: a nested class, the companion itself, or a
//! property-reference carrier. The outer class reads and writes its own field directly, so a
//! property used only there, or not at all, has no bridge.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Ledger {\n\
    fun total(): Int = base + step\n\
\n\
    fun bump() {\n\
        step = step + 1\n\
    }\n\
\n\
    companion object {\n\
        private val base = 10\n\
        private var step = 1\n\
    }\n\
}\n\
\n\
class Reader {\n\
    class Nested {\n\
        fun read(): String = label\n\
    }\n\
\n\
    companion object {\n\
        private val label = \"r\"\n\
        private val unused = \"u\"\n\
        private var mark = 0\n\
\n\
        fun markRef() = ::mark\n\
    }\n\
}\n\
";

#[test]
fn only_a_used_private_companion_field_gets_an_outer_bridge() {
    for class in [
        "store/Ledger",
        "store/Ledger$Companion",
        "store/Reader",
        "store/Reader$Nested",
        "store/Reader$Companion",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "CompanionFieldBridgeUse",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
