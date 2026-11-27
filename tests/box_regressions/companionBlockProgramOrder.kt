// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +CompanionBlocks
// A class's static state initializes in program order around its `companion object`: the block
// properties written before it, then the companion's construction, properties and `init` blocks,
// then the block properties written after it.

var log = ""

fun l(step: String): String {
    log += step
    return step
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

fun box(): String {
    A.b1
    log += "|"
    B.b
    return if (log == "b1o1oio2b2|ob") "OK" else log
}
