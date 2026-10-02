// Kotlin's answers for `range_kotlinc_oracle.c`, the questions of 463b8dae's oracle case: a stepped
// range's and a reversed progression's bounds, whether a progression and a range of the same
// elements compare equal each way, and membership in an unsigned progression whose bounds
// straddle the signed boundary.
fun box(): String = buildString {
    fun line(label: String, answer: Boolean) = appendLine("$label: $answer")
    val stepped = 1..10 step 2
    val reversed = (1..9 step 3).reversed()
    val progression = 1..3 step 1
    val range = 1..3
    val unsigned = Long.MAX_VALUE.toULong() - 1uL..Long.MAX_VALUE.toULong() + 5uL step 3
    line("(1..10 step 2).first == 1", stepped.first == 1)
    line("(1..10 step 2).last == 9", stepped.last == 9)
    line("(1..9 step 3).reversed().first == 7", reversed.first == 7)
    line("(1..9 step 3).reversed().last == 1", reversed.last == 1)
    line("(1..3 step 1) == 1..3", progression == range)
    line("1..3 != (1..3 step 1)", range != progression)
    line("Long.MAX_VALUE.toULong() + 2uL in the unsigned progression",
        Long.MAX_VALUE.toULong() + 2uL in unsigned)
}
