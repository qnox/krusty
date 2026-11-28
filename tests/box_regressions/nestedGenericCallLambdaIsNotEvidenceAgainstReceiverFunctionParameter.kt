class Sh

class Entry<out A, out B>(val first: A, val second: B)

infix fun <A, B> A.entry(that: B): Entry<A, B> = Entry(this, that)

fun Sh.chain(vararg branches: Entry<String, Sh.() -> Unit>): Int = branches.size

fun case(word: String, vararg branches: Entry<String, Sh.() -> Unit>): String = word + branches.size

fun box(): String {
    val total = Sh().chain("a" entry {}) + Sh().chain("b" entry {}, "c" entry {})
    return if (total == 3 && case("w", "e" entry {}) == "w1") "OK" else "FAIL: $total"
}
