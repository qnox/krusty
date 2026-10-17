//! A dependency's suspend function returning a nullable value class over a reference
//! (`suspend fun find(): Id?` with `value class Id(val value: String)`) hands over the carrier.
//! When that result goes straight into a generic parameter or receiver, the consumer still boxes
//! it, as kotlinc does. krusty paired the consumer's box with an unbox it never emits for such a
//! call, dropped both, and passed the bare `String`, so `find() shouldBe Id("x")` compared a
//! string with an `Id`.

use super::common;

const LIBRARY: &str = "package lib\n\
import kotlin.coroutines.*\n\
@JvmInline\n\
value class Id(val value: String)\n\
interface Resolver { suspend fun resolve(key: String): Id? }\n\
class Store : Resolver {\n\
    fun get(): Id? = Id(\"x\")\n\
    suspend fun find(): Id? = Id(\"x\")\n\
    override suspend fun resolve(key: String): Id? = key.let { Id(it) }\n\
}\n\
fun <T> runSync(block: suspend () -> T): T {\n\
    var out: Result<T>? = null\n\
    block.startCoroutine(Continuation(EmptyCoroutineContext) { out = it })\n\
    return out!!.getOrThrow()\n\
}\n";

const MAIN: &str = "import lib.*\n\
infix fun <T> T.shouldBe(expected: T?): String = if (this == expected) \"OK\" else \"was $this\"\n\
fun <T> same(actual: T, expected: T?): String = if (actual == expected) \"OK\" else \"was $actual\"\n\
fun box(): String {\n\
    val store = Store()\n\
    val results = listOf(\n\
        store.get() shouldBe Id(\"x\"),\n\
        runSync { store.find() shouldBe Id(\"x\") },\n\
        runSync { same(store.find(), Id(\"x\")) },\n\
        runSync { store.resolve(\"x\") shouldBe Id(\"x\") },\n\
        runSync { val found = store.find(); found shouldBe Id(\"x\") },\n\
    )\n\
    return results.firstOrNull { it != \"OK\" } ?: \"OK\"\n\
}\n";

#[test]
fn a_dependency_suspend_carrier_result_is_boxed_for_a_generic_consumer() {
    assert_eq!(
        common::expect_box_run_against_ref("suspend-carrier-rebox", LIBRARY, MAIN).as_deref(),
        Some("OK")
    );
}
