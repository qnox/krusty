// A negative `IntArray` length throws into the surrounding `catch`. The size expression runs once.

var evaluations = 0

fun length(): Int {
    evaluations += 1
    return -1
}

fun box(): String {
    val caught = try {
        IntArray(length())
        "fail"
    } catch (t: Throwable) {
        "OK"
    }
    if (caught != "OK") return caught
    if (evaluations != 1) return "evaluations=$evaluations"
    return "OK"
}
