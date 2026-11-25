// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +CompanionBlocks
// FILE: Main.kt
fun box(): String {
    // The log's own file is initialized before any class touches it, so the order below is the
    // classes' alone.
    log += ""
    // The first use of each class is a read of a block property declared in another file: it
    // initializes the superclass and its companion object first, and the class in program order.
    Derived.d
    log += "|"
    A.b2
    log += "|"
    B.b
    return if (log == "bwd|b1o1oio2b2|ob") "OK" else log
}

// FILE: Classes.kt
var log = ""

fun l(step: String): String {
    log += step
    return step
}

open class Base {
    companion { val b = l("b") }
    companion object { init { l("w") } }
}

class Derived : Base() {
    companion { val d = l("d") }
}

class A {
    companion { val b1 = l("b1") }
    companion object {
        val o1 = l("o1")
        init { l("oi") }
        val o2 = l("o2")
    }
    companion { val b2 = l("b2") }
}

class B {
    companion object { val o = l("o") }
    companion { val b = l("b") }
}
