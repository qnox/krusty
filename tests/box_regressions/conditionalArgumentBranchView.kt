// A branch of an `if` passed to a generic parameter keeps the argument view a whole argument gets
// (kotlinc 2.4.21): an integer literal adapts to a type an invariant argument fixes, through both
// function and constructor selection, and a callable reference converts to a fun interface.
fun <T> sameAs(items: MutableList<T>, item: T): T {
    items.add(item)
    return items[0]
}

class Pairing<T>(items: MutableList<T>, item: T) {
    val first: T = items.apply { add(item) }[0]
}

fun interface Source<T> {
    fun get(): T
}

fun <T> read(source: Source<T>): T = source.get()

fun one(): Int = 1

fun two(): Int = 2

fun box(): String {
    val long: Long = -10
    val int: Int = 20
    val fixed: Long = sameAs(mutableListOf<Long>(), if (int > 0) long else 2)
    val constructed: Long = Pairing(mutableListOf<Long>(), if (int < 0) long else 3).first
    val referenced: Int = read(if (int > 0) ::one else ::two)
    val result = "$fixed $constructed $referenced"
    return if (result == "-10 3 1") "OK" else "FAIL: $result"
}
