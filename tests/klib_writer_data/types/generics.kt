package ty
fun <T : Comparable<T>> maxOf2(a: T, b: T): T = if (a > b) a else b
fun <K, V> pick(m: Map<out K, V>, k: K): V? = m[k]
fun star(l: List<*>): Int = l.size
fun fn(f: (Int, String) -> Boolean, g: Int.() -> Unit): Unit {}
val <T> List<T>.second: T get() = this[1]
var counter = 0
    private set
fun <T> where(x: T): T where T : CharSequence, T : Comparable<T> = x
