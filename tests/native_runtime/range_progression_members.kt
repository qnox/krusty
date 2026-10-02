// Kotlin's answers for `range_progression_members.c`: a progression's `toString`, `equals` and
// `hashCode` include its step, and only the ranges `..` and `until` answer leave it out.
fun box(): String = buildString {
    appendLine("${1..10} | ${1..10 step 2} | ${10 downTo 1} | ${1..3 step 1} | " +
        "${(1..9 step 3).reversed()} | ${5L downTo 1L step 2} | ${'a'..'e' step 2} | " +
        "${'e' downTo 'a'}")
    appendLine("${(10 downTo 1) == (10..1)} ${(10..1) == (10 downTo 1)} " +
        "${(1..9 step 2) == (1..9 step 4)} ${(1..10 step 2) == (1..9 step 2)}")
    appendLine("${(1..3 step 1) == (1..3)} ${(1..3) == (1..3 step 1)} " +
        "${(1 downTo 10) == (5..1)} ${(5..1) == (1 downTo 10)} ${(1..3) == (1..3)}")
    appendLine("${(1..10).hashCode()} ${(1..10 step 2).hashCode()} " +
        "${(10 downTo 1).hashCode()} ${(5L downTo 1L step 2).hashCode()} " +
        "${('a'..'e' step 2).hashCode()} ${(1 downTo 10).hashCode()}")
}
