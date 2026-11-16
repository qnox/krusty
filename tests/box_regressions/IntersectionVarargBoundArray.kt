interface Base
interface Mid : Base
interface Zed : Base
class X : Mid, Zed
class Y : Mid, Zed

fun <T : Zed> bounded(vararg values: T): Int = values.size
fun <T : Number> numbers(vararg values: T): Int = values.size
fun <T> unbounded(vararg values: T): Int = values.size

fun box(): String {
    val total = bounded(X(), Y()) + numbers(1, 4.5) + unbounded(X(), Y()) + unbounded("a", 1)
    return if (total == 8) "OK" else "Fail: $total"
}
