// TARGET_BACKEND: JVM_IR, WASM
// PLATFORM-SPECIFIC: without CompanionBlocks Native initializes a class's companion before its
// superclass's, as `nativeCompanionInitOrderLegacy` pins; the shared program with the feature
// enabled is companionBeforeConstructorArgumentWithCompanionBlocks.
// `new` initializes the class before constructor arguments are evaluated. The companion
// runs, then the argument, then the instance initializer.

var log = ""

class Holder(n: Int) {
    init { log += "I" }
    companion object { init { log += "C" } }
}

fun argument(): Int { log += "A"; return 0 }

fun box(): String {
    Holder(argument())
    return if (log == "CAI") "OK" else log
}
