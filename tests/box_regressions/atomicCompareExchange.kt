@file:OptIn(kotlin.concurrent.atomics.ExperimentalAtomicApi::class)
import kotlin.concurrent.atomics.*

fun exchangeInt(a: AtomicInt, expected: Int, value: Int): Int = a.compareAndExchange(expected, value)
fun exchangeLong(a: AtomicLong, expected: Long, value: Long): Long = a.compareAndExchange(expected, value)
fun exchangeBoolean(a: AtomicBoolean, expected: Boolean, value: Boolean): Boolean = a.compareAndExchange(expected, value)
fun exchangeReference(a: AtomicReference<String>, expected: String, value: String): String = a.compareAndExchange(expected, value)
fun exchangeIntArray(a: AtomicIntArray, index: Int, expected: Int, value: Int): Int = a.compareAndExchangeAt(index, expected, value)
fun exchangeLongArray(a: AtomicLongArray, index: Int, expected: Long, value: Long): Long = a.compareAndExchangeAt(index, expected, value)
fun exchangeArray(a: AtomicArray<String>, index: Int, expected: String, value: String): String = a.compareAndExchangeAt(index, expected, value)

class UserExchange {
    fun compareAndExchange(expected: Int, value: Int): Int = expected * 10 + value
}

fun box(): String {
    if (UserExchange().compareAndExchange(2, 3) != 23) return "user declaration"
    val i = AtomicInt(1)
    if (exchangeInt(i, 0, 2) != 1 || i.load() != 1) return "int failure"
    if (exchangeInt(i, 1, 2) != 1 || i.load() != 2) return "int success"
    val l = AtomicLong(1L)
    if (exchangeLong(l, 0L, 2L) != 1L || l.load() != 1L) return "long failure"
    if (exchangeLong(l, 1L, 2L) != 1L || l.load() != 2L) return "long success"
    val b = AtomicBoolean(false)
    if (exchangeBoolean(b, true, true) || b.load()) return "boolean failure"
    if (exchangeBoolean(b, false, true) || !b.load()) return "boolean success"
    val x = "x"
    val y = "y"
    val r = AtomicReference(x)
    if (exchangeReference(r, y, y) !== x || r.load() !== x) return "reference failure"
    if (exchangeReference(r, x, y) !== x || r.load() !== y) return "reference success"
    val ia = AtomicIntArray(1)
    if (exchangeIntArray(ia, 0, 1, 2) != 0 || ia.loadAt(0) != 0) return "int array failure"
    if (exchangeIntArray(ia, 0, 0, 2) != 0 || ia.loadAt(0) != 2) return "int array success"
    val la = AtomicLongArray(1)
    if (exchangeLongArray(la, 0, 1L, 2L) != 0L || la.loadAt(0) != 0L) return "long array failure"
    if (exchangeLongArray(la, 0, 0L, 2L) != 0L || la.loadAt(0) != 2L) return "long array success"
    val ra = AtomicArray(arrayOf(x))
    if (exchangeArray(ra, 0, y, y) !== x || ra.loadAt(0) !== x) return "reference array failure"
    if (exchangeArray(ra, 0, x, y) !== x || ra.loadAt(0) !== y) return "reference array success"
    return "OK"
}
