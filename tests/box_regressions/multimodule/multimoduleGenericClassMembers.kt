// MODULE: lib
open class Crate<T> {
    fun echo(value: T): T = value
    fun <R> choose(other: R, value: T): T = value
    fun defaultValue(): T? = null
}
class Inherited : Crate<String>()
class Outer<T> {
    inner class Captured {
        fun echo(value: T): T = value
    }
    inner class Own<U> {
        fun outer(value: T): T = value
        fun own(value: U): U = value
    }
}

fun ints(): Crate<Int> = Crate()
fun strings(): Crate<String> = Crate()
fun longs(): Crate<Long> = Crate()
fun nullable(): Crate<String?> = Crate()
fun inherited(): Inherited = Inherited()
fun captured(): Outer<String>.Captured = Outer<String>().Captured()
fun ownAndCaptured(): Outer<Int>.Own<String> = Outer<Int>().Own<String>()

// MODULE: main(lib)
fun box(): String {
    if (ints().echo(41) + 1 != 42) return "integer"
    if (longs().echo(4294967297L) != 4294967297L) return "long"
    if (nullable().echo(null) != null) return "nullable"
    if (ints().choose("other", 42) != 42) return "class-and-method"
    if (inherited().echo("I") != "I") return "inherited"
    if (inherited().defaultValue() != null) return "inherited-result"
    if (strings().defaultValue() != null) return "zero-argument"
    if (captured().echo("C") != "C") return "captured-only"
    if (ownAndCaptured().outer(42) != 42) return "captured-outer"
    if (ownAndCaptured().own("U") != "U") return "own-and-captured"
    return strings().choose(7, strings().echo("O")) + strings().echo("K")
}
