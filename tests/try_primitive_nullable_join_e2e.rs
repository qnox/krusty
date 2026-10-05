//! A `try` used as a value whose body yields a PRIMITIVE and whose catch yields `null`
//! (`try { s.length } catch (e: Exception) { null }`) is `Int?`, exactly as `if (c) s.length else
//! null` is. The checker joined only two reference branches and collapsed a primitive beside a
//! reference to `Unit`, so every consumer expecting `Int?` — a declared return, a lambda passed to
//! `map`/`mapNotNull` or to a repository-owned function — reported "return type mismatch: expected
//! 'Int?', actual 'Unit'". The suspend forms resume through a parked continuation so the callee
//! really suspends inside the protected region.
use super::common;

#[test]
fn primitive_try_with_null_catch_is_nullable_primitive() {
    const MAIN: &str = r#"fun length(s: String): Int? = try { if (s.isEmpty()) throw IllegalStateException() else s.length } catch (e: Exception) { null }

fun <R> apply(s: String, f: (String) -> R): R = f(s)

fun box(): String {
    if (length("abc") != 3 || length("") != null) return "declared ${length("abc")} ${length("")}"
    val local = try { "xy".length } catch (e: Exception) { null }
    if (local != 2) return "local $local"
    val inferred = apply("abcd") { s -> try { s.length } catch (e: Exception) { null } }
    if (inferred != 4) return "inferred $inferred"
    val reversed = apply("") { s -> try { null } catch (e: Exception) { s.length } }
    if (reversed != null) return "reversed $reversed"
    val mapped = listOf("a", "", "ccc").map { s -> try { if (s.isEmpty()) error("e") else s.length } catch (e: Exception) { null } }
    if (mapped != listOf(1, null, 3)) return "map $mapped"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(MAIN, "Main");
}

#[test]
fn suspending_primitive_try_in_a_map_not_null_lambda_matches_kotlinc() {
    const MAIN: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

var parked: Continuation<Int>? = null

class Source {
    suspend fun fetch(item: String): Int {
        if (item.isEmpty()) throw IllegalArgumentException("empty")
        return suspendCoroutineUninterceptedOrReturn { c -> parked = c; COROUTINE_SUSPENDED }
    }
}

inline fun <T, R : Any> List<T>.keepNotNull(f: (T) -> R?): List<R> {
    val out = ArrayList<R>()
    for (x in this) {
        val r = f(x)
        if (r != null) out.add(r)
    }
    return out
}

suspend fun stdlib(source: Source, items: List<String>): List<Int> =
    items.mapNotNull { item -> try { source.fetch(item) } catch (e: Exception) { null } }

suspend fun owned(source: Source, items: List<String>): List<Int> =
    items.keepNotNull { item -> try { source.fetch(item) } catch (e: Exception) { null } }

fun drive(block: suspend () -> List<Int>): String {
    var outcome: Result<List<Int>>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) { outcome = it })
    var next = 10
    while (true) {
        val c = parked ?: break
        parked = null
        c.resume(next++)
    }
    return outcome!!.getOrThrow().toString()
}

fun box(): String {
    val items = listOf("a", "", "b")
    val a = drive { stdlib(Source(), items) }
    if (a != "[10, 11]") return "stdlib $a"
    val b = drive { owned(Source(), items) }
    if (b != "[10, 11]") return "owned $b"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(MAIN, "Main");
}
