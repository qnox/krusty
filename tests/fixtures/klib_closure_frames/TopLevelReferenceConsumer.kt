var referenceFactoryCalls = 0
var referenceArgumentCalls = 0

fun nextReference(): (Int) -> Int {
    referenceFactoryCalls++
    return makeReference()
}

fun referenceArgument(): Int {
    referenceArgumentCalls++
    return 41
}

fun box(): String {
    val first = nextReference()
    val second = nextReference()
    val prepared = inlineMakeReference()
    val other = makeOtherReference()
    if (first != second || first != prepared) return "reference identity"
    if (first == other) return "different declaration"
    if (first(referenceArgument()) != 42) return "first invocation"
    if (second(referenceArgument()) != 42) return "second invocation"
    if (prepared(referenceArgument()) != 42) return "prepared invocation"
    if (other(40) != 42) return "other invocation"
    if (referenceFactoryCalls != 2) return "factory evaluation"
    if (referenceArgumentCalls != 3) return "argument evaluation"
    return "OK"
}
