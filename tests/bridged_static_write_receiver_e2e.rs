//! A write to another class's private static field through its synthetic setter keeps the
//! singleton receiver as a statement: kotlinc's `JvmPropertiesLowering.patchReceiver` turns the
//! static store's receiver into `getstatic <singleton>; pop` ahead of the value. A read through
//! the getter bridge, a store the owner makes directly, and a `this` receiver drop it.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Vault {\n\
    class Teller {\n\
        fun deposit() { code = \"OK\" }\n\
        fun peek(): String = code\n\
        fun qualified() { Vault.code = \"Q\" }\n\
        fun add() { total += 2 }\n\
    }\n\
\n\
    fun reset() { code = \"own\" }\n\
\n\
    companion object {\n\
        private var code = \"none\"\n\
        private var total = 0\n\
        fun local() { code = \"c\" }\n\
        fun named() { Vault.code = \"n\" }\n\
    }\n\
}\n\
\n\
object Registry {\n\
    private var entry = \"x\"\n\
    fun own() { Registry.entry = \"o\" }\n\
    class Clerk {\n\
        fun write() { entry = \"y\" }\n\
        fun read() = entry\n\
    }\n\
}\n\
";

#[test]
fn a_bridged_static_write_evaluates_its_singleton_receiver() {
    for class in [
        "store/Vault",
        "store/Vault$Teller",
        "store/Vault$Companion",
        "store/Registry",
        "store/Registry$Clerk",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "BridgedStaticWriteReceiver",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
