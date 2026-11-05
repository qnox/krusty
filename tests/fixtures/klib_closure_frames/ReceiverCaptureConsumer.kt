var captureEffects = 0

fun receiverInput(value: Int): Int {
    captureEffects = captureEffects * 10 + 1
    return value
}

fun seedInput(value: Int): Int {
    captureEffects = captureEffects * 10 + 2
    return value
}

fun box(): String {
    val first = receiverInput(5).capture(seedInput(3))
    if (captureEffects != 12) return "first input order"
    val second = receiverInput(11).capture(seedInput(7))
    if (captureEffects != 1212) return "second input order"
    if (first() != 8 || second() != 18 || first() != 8 || second() != 18) return "capture isolation"
    if (captureEffects != 1212) return "inputs evaluated again"
    return "OK"
}
