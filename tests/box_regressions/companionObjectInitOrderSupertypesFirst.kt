// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +CompanionBlocks
var log = ""

fun note(step: String) {
    log += step
}

interface Named {
    fun name(): String = "named"
    companion object { init { note("N") } }
}

interface Abstract {
    fun other(): String
    companion object { init { note("X") } }
}

open class Root {
    companion object { init { note("R") } }
}

open class Middle : Root()

class Leaf : Middle(), Named, Abstract {
    override fun other(): String = "other"
    companion object { init { note("L") } }
}

open class Other {
    companion object { init { note("O") } }
}

class Plain : Other() {
    init { note("p") }
    companion object { init { note("P") } }
}

fun box(): String {
    log = ""
    Leaf
    if (log != "RNL") return "companion access: $log"

    log = ""
    Plain()
    if (log != "OPp") return "construction: $log"
    return "OK"
}
