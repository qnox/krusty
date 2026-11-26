// LANGUAGE: +ContextParameters
// MODULE: lib
// FILE: Scale.kt
abstract class Scale {
    context(unit: Int)
    abstract val String.weight: Int

    context(unit: Int)
    fun weigh(text: String): Int = text.weight
}

// MODULE: box(lib)
// FILE: box.kt
class Doubling : Scale() {
    context(unit: Int)
    override val String.weight: Int get() = length * unit
}

fun box(): String {
    val weight = with(2) { Doubling().weigh("abc") }
    return if (weight == 6) "OK" else "fail: $weight"
}
