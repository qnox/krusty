//! Where a call passing implicit context arguments starts its line.
//!
//! kotlinc reads a dispatch, extension or lambda receiver passed as a context argument at the
//! call's own source offsets, so the call's line begins at that load. A context parameter passed on
//! is read with no offsets, so the line begins at the first written argument, or at the dispatch
//! when the call has none, also in a lifted local function, where no declaration line stands in
//! for the withdrawn start. Each class below must be kotlinc's, debug tables included.
use super::common;

const SOURCE: &str = "context(c: String)\n\
    fun greet(n: Int): Int = c.length + n\n\
    context(c: String)\n\
    fun size(): Int = c.length\n\
    fun fromLambdaReceiver(): Int = with(\"tt\") {\n\
    \x20   greet(1)\n\
    }\n\
    fun fromLambdaReceiverOnly(): Int = with(\"q\") {\n\
    \x20   size()\n\
    }\n\
    fun String.fromExtensionReceiver(): Int {\n\
    \x20   return greet(2)\n\
    }\n\
    context(c: String)\n\
    fun fromContextParameter(): Int {\n\
    \x20   return greet(3)\n\
    }\n\
    context(c: String)\n\
    fun fromContextParameterOnly(): Int {\n\
    \x20   return size()\n\
    }\n\
    context(_: String)\n\
    fun fromAnonymousContextParameterOnly(): Int {\n\
    \x20   return size()\n\
    }\n\
    class Holder {\n\
    \x20   fun fromDispatchReceiver(): Int {\n\
    \x20       return useHolder()\n\
    \x20   }\n\
    }\n\
    context(h: Holder)\n\
    fun useHolder(): Int = 1\n\
    context(c: String)\n\
    fun fromLiftedLocal(): Int {\n\
    \x20   fun local(): Int {\n\
    \x20       return size()\n\
    \x20   }\n\
    \x20   return local()\n\
    }\n\
    context(_: String)\n\
    fun fromLiftedLocalAnonymous(): Int {\n\
    \x20   fun local(): Int {\n\
    \x20       return size()\n\
    \x20   }\n\
    \x20   return local()\n\
    }\n\
    fun box(): String {\n\
    \x20   val total = fromLambdaReceiver() + fromLambdaReceiverOnly() + \"ab\".fromExtensionReceiver() +\n\
    \x20       with(\"abc\") { fromContextParameter() + fromContextParameterOnly() + fromAnonymousContextParameterOnly() } +\n\
    \x20       with(\"ab\") { fromLiftedLocal() + fromLiftedLocalAnonymous() } +\n\
    \x20       Holder().fromDispatchReceiver()\n\
    \x20   return if (total == 3 + 1 + 4 + 6 + 3 + 3 + 2 + 2 + 1) \"OK\" else \"Fail $total\"\n\
    }\n";

#[test]
fn implicit_context_calls_start_their_line_where_kotlinc_does() {
    common::assert_class_matches_kotlinc("ContextArgumentLines", SOURCE, "ContextArgumentLinesKt");
    common::assert_class_matches_kotlinc("ContextArgumentLines", SOURCE, "Holder");
    common::expect_box_same_as_kotlinc(SOURCE, "ContextArgumentLinesRun");
}
