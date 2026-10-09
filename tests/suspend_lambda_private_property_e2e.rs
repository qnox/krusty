//! A suspend lambda that reads a private property of its enclosing class. The JVM backend makes such
//! a lambda a `SuspendLambda` class of its own, so the read crosses classes and needs the
//! `access$getX$p` bridge, exactly as kotlinc emits. Calling the private getter directly failed at
//! run time with `NoSuchMethodError`. An inline lambda stays in the caller's class and needs no bridge.
use super::common;

const SOURCE: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.EmptyCoroutineContext\n\
import kotlin.coroutines.startCoroutine\n\
class Manager { fun get(key: String) = \"got $key\" }\n\
fun <T> call(block: () -> T): T = block()\n\
fun <T> suspended(block: suspend () -> T): T {\n\
    var result: Result<T>? = null\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it })\n\
    return result!!.getOrThrow()\n\
}\n\
class Holder {\n\
    private val manager = Manager()\n\
    private var count = 0\n\
    fun plain(key: String): String = call { manager.get(key) }\n\
    fun expression(key: String): String = suspended { manager.get(key) }\n\
    fun statement(key: String): String { return suspended { count += 1; manager.get(key) + count } }\n\
}\n\
fun box(): String {\n\
    val holder = Holder()\n\
    val result = holder.plain(\"a\") + \",\" + holder.expression(\"b\") + \",\" + holder.statement(\"c\")\n\
    return if (result == \"got a,got b,got c1\") \"OK\" else \"fail: $result\"\n\
}\n";

#[test]
fn a_suspend_lambda_reads_an_enclosing_private_property_through_its_bridge() {
    common::expect_box_same_as_kotlinc(SOURCE, "SuspendLambdaPrivateProperty");
}
