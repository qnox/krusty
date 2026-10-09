//! An inline function that returns early and otherwise returns through a cleanup lambda (`use`),
//! inlined with a `Unit` lambda. The cleanup `try` lands `Unit.INSTANCE` physically; its consumer
//! must see that value instead of the semantic `Unit`, or the two return paths join with different
//! stack heights and frame computation panics.
use super::common;

const SOURCE: &str = "class Session : AutoCloseable { override fun close() { log += \"c\" } }\n\
var log = \"\"\n\
inline fun <R> withSession(early: Boolean, block: () -> R): R {\n\
    if (early) return block()\n\
    return Session().use { block() }\n\
}\n\
fun session(early: Boolean) = withSession(early) { log += \"s\" }\n\
fun value(early: Boolean): String = withSession(early) { \"v\" }\n\
fun box(): String {\n\
    session(true); session(false)\n\
    val result = log + value(true) + value(false)\n\
    return if (result == \"sscvv\") \"OK\" else \"fail: $result\"\n\
}\n";

#[test]
fn an_early_return_joins_a_unit_cleanup_result() {
    common::expect_box_same_as_kotlinc(SOURCE, "InlineEarlyReturnCleanup");
}
