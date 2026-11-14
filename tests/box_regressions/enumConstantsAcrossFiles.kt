// FILE: EarlyReader.kt
// Reads constants before the file declaring their enum: each read is linked by the constant,
// not by the order the files are lowered in.
fun early(): Signal = Signal.GO

fun earlyWait(): Any = Signal.WAIT

// FILE: Signal.kt
enum class Signal(val weight: Int) {
    STOP(1),
    WAIT(2) { override fun score(): Int = weight * 10 },
    GO(3);

    open fun score(): Int = weight
}

fun declaringFile(): Signal = Signal.GO

fun scores(): Boolean = Signal.STOP.score() == 1 && Signal.WAIT.score() == 20 && Signal.GO.score() == 3

// FILE: LateReader.kt
// Reads constants after the declaring file.
fun late(): Signal = Signal.GO

fun box(): String {
    if (early() !== late() || early() !== declaringFile()) return "Fail: one GO"
    if (earlyWait() !== Signal.WAIT) return "Fail: one WAIT"
    if (early().name != "GO" || early().ordinal != 2 || early().weight != 3) return "Fail: GO fields"
    if (Signal.WAIT.name != "WAIT" || Signal.WAIT.ordinal != 1 || Signal.WAIT.weight != 2) return "Fail: WAIT fields"
    if (!scores()) return "Fail: scores"
    val erased = earlyWait()
    if (erased.hashCode() != erased.hashCode()) return "Fail: identity hash"
    return "OK"
}
