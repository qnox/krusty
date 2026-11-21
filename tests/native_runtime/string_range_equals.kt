// Kotlin's answers for `string_range_equals.c`: `regionMatches` over ranges inside both strings,
// which is exactly what Kotlin/Native's `unsafeRangeEquals` is asked once the bounds are checked.
fun box(): String = buildString {
    appendLine("0: " + "kotlin native".regionMatches(7, "a native b", 2, 6))
    appendLine("1: " + "kotlin native".regionMatches(0, "kotlix", 0, 6))
    appendLine("2: " + "kotlin native".regionMatches(3, "lin", 0, 0))
    appendLine("3: " + "kotlin native".regionMatches(10, "tive", 0, 3))
    appendLine("4: " + "a\uD83D\uDE00b".regionMatches(2, "\uDE00b", 0, 2))
    appendLine("5: " + "a\uD83D\uDE00b".regionMatches(1, "\uD83D\uDE00", 0, 2))
    appendLine("6: " + "a\uD83D\uDE00b".regionMatches(1, "\uD83D\uDE01", 0, 2))
    appendLine("7: " + "a\uD83D\uDE00b".regionMatches(1, "x\uD83D", 1, 1))
    appendLine("8: " + "\u00E9t\u00E9".regionMatches(1, "t\u00E9", 0, 2))
}
