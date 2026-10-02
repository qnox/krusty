// Kotlin's answers for `range_contains_unsigned.c`: `value in progression` and `value in range` at
// the extremes. A progression's `in` is `Iterable.contains`, a walk with an `Int` index; each line
// is the question and `true`, `false` or the exception it threw.
//
// Questions whose walk reaches index 2^31 are not asked here: each walks 2^31 elements on the JVM,
// 5 to 13 seconds apiece, past the harness's 10-second limit on one `box()`. kotlinc 2.4.20
// answers them, and the driver pins them, as:
//     9223372036854775809uL in (0uL..ULong.MAX_VALUE step 3)        threw ArithmeticException
//     ULong.MAX_VALUE in (0uL..ULong.MAX_VALUE step 3)              threw ArithmeticException
//     4uL in (ULong.MAX_VALUE downTo 0uL step 3)                    threw ArithmeticException
//     0L in (Long.MIN_VALUE..Long.MAX_VALUE step 3)                 threw ArithmeticException
//     -1L in (0L..2147483647L step 1)                               false
//     2147483647L in (0L..2147483647L step 1)                       true
//     2147483647L in (0L..2147483648L step 1)                       true
//     2147483648L in (0L..2147483648L step 1)                       threw ArithmeticException
//     -5L in (0L..2147483648L step 1)                               threw ArithmeticException
// each exception being `ArithmeticException("Index overflow has happened.")`.
fun box(): String = buildString {
    fun t(label: String, question: () -> Boolean) {
        val answer = try {
            question().toString()
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label $answer")
    }
    val first = 9223372036854775806uL
    val p = first..(first + 12uL) step 3
    t("9223372036854775809uL in p") { 9223372036854775809uL in p }
    t("9223372036854775808uL in p") { 9223372036854775808uL in p }
    t("9223372036854775813uL in p") { 9223372036854775813uL in p }
    t("9223372036854775814uL in p") { 9223372036854775814uL in p }
    val q = (first + 12uL) downTo first step 3
    t("9223372036854775812uL in q") { 9223372036854775812uL in q }
    t("9223372036854775811uL in q") { 9223372036854775811uL in q }
    t("3uL in (0uL..ULong.MAX_VALUE step 3)") { 3uL in (0uL..ULong.MAX_VALUE step 3) }
    t("ULong.MAX_VALUE - 3uL in (ULong.MAX_VALUE downTo 0uL step 3)") {
        ULong.MAX_VALUE - 3uL in (ULong.MAX_VALUE downTo 0uL step 3)
    }
    val uints = 1u..4000000000u step 7
    t("2999999997u in (1u..4000000000u step 7)") { 2999999997u in uints }
    t("3000000000u in (1u..4000000000u step 7)") { 3000000000u in uints }
    t("3000000000u in 1u..4000000000u") { 3000000000u in 1u..4000000000u }
    t("ULong.MAX_VALUE in 0uL..ULong.MAX_VALUE") { ULong.MAX_VALUE in 0uL..ULong.MAX_VALUE }
    t("Long.MIN_VALUE + 3 in (Long.MIN_VALUE..Long.MAX_VALUE step 3)") {
        Long.MIN_VALUE + 3 in (Long.MIN_VALUE..Long.MAX_VALUE step 3)
    }
    t("0L in Long.MIN_VALUE..Long.MAX_VALUE") { 0L in Long.MIN_VALUE..Long.MAX_VALUE }
    t("5 in (10 downTo 1 step 2)") { 5 in (10 downTo 1 step 2) }
    t("4 in (10 downTo 1 step 2)") { 4 in (10 downTo 1 step 2) }
    t("-3 in (-10..10 step 7)") { -3 in (-10..10 step 7) }
    t("10 in (-10..10 step 7)") { 10 in (-10..10 step 7) }
    t("5 in 10 downTo 1") { 5 in 10 downTo 1 }
    t("5L in (10L..0L step 1)") { 5L in (10L..0L step 1) }
}
