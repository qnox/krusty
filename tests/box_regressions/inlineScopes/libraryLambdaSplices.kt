// TARGET_BACKEND: JVM_IR
// USE_INLINE_SCOPES_NUMBERS
// WITH_STDLIB
fun total(numbers: List<Int>): Int {
    var sum = 0
    numbers.forEach { n -> sum += n }
    return sum
}

fun twice(numbers: List<Int>): Int {
    var sum = 0
    numbers.forEach { sum += it }
    numbers.forEach { n -> sum += n * n }
    return sum
}

fun box(): String {
    val numbers = listOf(1, 2, 3)
    if (total(numbers) != 6) return "fail total"
    if (twice(numbers) != 20) return "fail twice"
    return "OK"
}
