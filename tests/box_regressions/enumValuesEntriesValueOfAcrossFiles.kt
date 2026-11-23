// WITH_STDLIB
// FILE: EarlyReader.kt
// Reads `values()`, `entries` and `valueOf` of an enum before the file declaring it: each is linked
// by the enum, not by the order the files are lowered in.
@OptIn(ExperimentalStdlibApi::class)
fun earlyEntries(): List<Phase> = Phase.entries

fun earlyValues(): Array<Phase> = Phase.values()

fun earlyValueOf(name: String): Phase = Phase.valueOf(name)

// FILE: Phase.kt
enum class Phase(val hours: Int) {
    DAWN(6),
    NOON(12) { override fun label(): String = "midday" },
    DUSK(18);

    open fun label(): String = name.lowercase()
}

@OptIn(ExperimentalStdlibApi::class)
fun declaringEntries(): List<Phase> = Phase.entries

// Inlined into the reading files, which then read the members of an enum another file declares.
@OptIn(ExperimentalStdlibApi::class)
inline fun inlinedEntries(): List<Phase> = Phase.entries

inline fun inlinedValues(): Array<Phase> = Phase.values()

inline fun inlinedValueOf(name: String): Phase = Phase.valueOf(name)

// No use in this file: only a later file reads its members.
enum class Tide { LOW, HIGH }

// FILE: LateReader.kt
// Reads them after the declaring file.
import kotlin.enums.enumEntries

@OptIn(ExperimentalStdlibApi::class)
fun box(): String {
    val entries = Phase.entries
    if (entries !== earlyEntries() || entries !== declaringEntries()) return "Fail: one entries list"
    if (entries !== inlinedEntries() || entries !== enumEntries<Phase>()) return "Fail: inlined entries"
    if (entries.toString() != "[DAWN, NOON, DUSK]") return "Fail: entries $entries"
    if (entries[1] !== Phase.NOON || entries[1].label() != "midday") return "Fail: entries[1]"

    val values = Phase.values()
    if (values === earlyValues()) return "Fail: values() is a fresh array"
    if (values.size != 3 || values[0] !== Phase.DAWN || values[2] !== Phase.DUSK) return "Fail: values"
    values[0] = Phase.DUSK
    if (earlyValues()[0] !== Phase.DAWN || inlinedValues()[0] !== Phase.DAWN) return "Fail: values() copy"
    if (earlyValues().joinToString { it.name } != "DAWN, NOON, DUSK") return "Fail: early values"

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
    if (Tide.entries.toString() != "[LOW, HIGH]" || Tide.entries !== Tide.entries) return "Fail: Tide entries"
    if (Tide.values().size != 2 || Tide.valueOf("HIGH") !== Tide.HIGH) return "Fail: Tide members"
    return "OK"
}
