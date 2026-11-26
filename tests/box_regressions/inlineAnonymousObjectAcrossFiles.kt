// WITH_STDLIB
// FILE: lib.kt
interface Maker { fun make(): String }

inline fun build(crossinline f: () -> String): Maker = object : Maker {
    override fun make() = f()
}

fun direct() = build { "O" }.make()

// FILE: other.kt
fun viaOther() = build { "K" }.make()

// FILE: main.kt
fun box() = direct() + viaOther()
