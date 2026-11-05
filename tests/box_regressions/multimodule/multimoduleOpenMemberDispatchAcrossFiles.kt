// Two files of one module each declare an overridable member of the same name. Each member's
// dispatching entry is its own declaration: another module's call through either one runs its
// receiver's override.
// MODULE: lib
// FILE: first.kt
open class First {
    open fun foo(x: Int): String = "first $x"
}
class FirstOverride : First() {
    override fun foo(x: Int): String = "O"
}
fun first(): First = FirstOverride()
// FILE: second.kt
open class Second {
    open fun foo(x: Int): String = "second $x"
}
class SecondOverride : Second() {
    override fun foo(x: Int): String = "K"
}
fun second(): Second = SecondOverride()
// MODULE: main(lib)
// FILE: main.kt
fun box(): String = first().foo(1) + second().foo(2)
