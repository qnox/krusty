// WITH_STDLIB
// FILE: early.kt
// Dispatches to members of an enum a later file declares. The enum's table type is the module's:
// the support units lowered before the declaring file must not give that file a second one.
fun member(e: Phase): Int = e.offset(42)

fun earlyValues(): Array<Phase> = Phase.values()

// FILE: phase.kt
enum class Phase(val hours: Int) {
    DAWN(6),
    NOON(12) { override fun label(): String = "midday" },
    DUSK(18);

    fun offset(a: Int): Int = a + hours

    open fun label(): String = name.lowercase()
}

// FILE: late.kt
fun box(): String {
    if (member(Phase.DAWN) != 48) return "FAIL member"
    if (Phase.NOON.label() != "midday" || Phase.DUSK.label() != "dusk") return "FAIL label"
    val names = earlyValues().joinToString { it.name }
    if (names != "DAWN, NOON, DUSK") return "FAIL $names"
    return "OK"
}
