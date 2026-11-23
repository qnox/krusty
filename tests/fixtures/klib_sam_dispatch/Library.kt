package fixture

interface Left<T> {
    fun apply(value: T): T
}

interface Right<T> {
    fun apply(value: T): T
}

fun interface Diamond : Left<String>, Right<String>

fun interface Specialized : Left<Int> {
    override fun apply(value: Int): Int
}

fun interface Defaulted<T> {
    fun apply(value: T): T
    fun note(): String = "default"
}

fun diamond(): Diamond = Diamond { it }
fun specialized(): Specialized = Specialized { it + 1 }
fun defaulted(): Defaulted<String> = Defaulted<String> { it }
inline fun inlineSpecialized(): Specialized = Specialized { it + 2 }

fun interface Printable {
    override fun toString(): String
}
fun printable(): Printable = Printable { "printed" }
