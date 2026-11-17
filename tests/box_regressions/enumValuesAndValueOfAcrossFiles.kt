// FILE: EarlyReader.kt
// Calls `values()` and `valueOf` of an enum before the file declaring it: each is linked by the
// enum, not by the order the files are lowered in.
fun earlyValues(): Array<Phase> = Phase.values()

fun earlyValueOf(name: String): Phase = Phase.valueOf(name)

// FILE: Phase.kt
enum class Phase(val hours: Int) {
    DAWN(6),
    NOON(12) { override fun label(): String = "midday" },
    DUSK(18);

    open fun label(): String = name + "!"
}

fun declaringValues(): Array<Phase> = Phase.values()

// Inlined into the reading files, which then call the members of an enum another file declares.
inline fun inlinedValues(): Array<Phase> = Phase.values()

inline fun inlinedValueOf(name: String): Phase = Phase.valueOf(name)

// No use in this file: only a later file calls its members.
enum class Tide { LOW, HIGH }

// FILE: LateReader.kt
// Calls them after the declaring file.
fun box(): String {
    val values = Phase.values()
    if (values === earlyValues() || values === declaringValues()) return "Fail: values() is a fresh array"
    if (values.size != 3 || values[0] !== Phase.DAWN || values[1] !== Phase.NOON || values[2] !== Phase.DUSK) {
        return "Fail: values"
    }
    if (values[1].label() != "midday" || values[2].label() != "DUSK!") return "Fail: labels"
    values[0] = Phase.DUSK
    if (earlyValues()[0] !== Phase.DAWN || inlinedValues()[0] !== Phase.DAWN) return "Fail: values() copies"
    var names = ""
    for (phase in earlyValues()) names += phase.name
    if (names != "DAWNNOONDUSK") return "Fail: early values $names"
    if (declaringValues().size != 3) return "Fail: declaring values"

    if (Phase.valueOf("DUSK") !== Phase.DUSK || earlyValueOf("NOON") !== Phase.NOON) return "Fail: valueOf"
    if (inlinedValueOf("DAWN").hours != 6) return "Fail: inlined valueOf"
    try {
        earlyValueOf("MIDNIGHT")
        return "Fail: valueOf of no constant"
    } catch (e: IllegalArgumentException) {
    }
    try {
        inlinedValueOf("dawn")
        return "Fail: inlined valueOf of no constant"
    } catch (e: IllegalArgumentException) {
    }

    val tides = Tide.values()
    if (tides.size != 2 || tides[1] !== Tide.HIGH || Tide.valueOf("LOW") !== Tide.LOW) return "Fail: Tide"
    return "OK"
}
