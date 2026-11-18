// An enum's own entry outranks a same-named entry imported explicitly, so a `when` in its companion
// yields the enum type rather than a join of two enum types.

// FILE: other/Verdict.kt
package other

enum class Verdict { UNKNOWN, EXPECTED }

// FILE: main.kt
import other.Verdict.UNKNOWN

class Trace {
    enum class RootType {
        FIRST, UNKNOWN, LAST;

        companion object {
            fun from(i: Int): RootType = when (i) {
                0 -> FIRST
                1 -> UNKNOWN
                else -> LAST
            }
        }
    }
}

fun box(): String {
    val entry = Trace.RootType.from(1)
    if (entry != Trace.RootType.UNKNOWN) return "fail: $entry"
    return if (UNKNOWN == other.Verdict.UNKNOWN) "OK" else "fail: import"
}
