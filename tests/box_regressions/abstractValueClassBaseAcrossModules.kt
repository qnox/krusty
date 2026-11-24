// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +FullValueClasses
// WITH_STDLIB

// MODULE: lib
// FILE: lib.kt

abstract value class Base(tag: Int) {
    abstract fun score(): Int
}

// MODULE: main(lib)
// FILE: main.kt

class Derived(private val tag: Int) : Base(tag) {
    override fun score(): Int = tag * 2
}

value class Pair(val first: Int, val second: Int) : Base(first) {
    override fun score(): Int = first * 10 + second
}

fun box(): String {
    val derived: Base = Derived(21)
    if (derived.score() != 42) return "FAIL1"
    val pair: Base = Pair(4, 2)
    if (pair.score() != 42) return "FAIL2"
    return "OK"
}
