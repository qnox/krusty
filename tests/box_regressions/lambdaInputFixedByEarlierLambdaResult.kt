fun <T> measure(make: () -> T, size: (T) -> Int): Int = size(make())

fun box(): String {
    val length = measure({ "four" }, { value -> value.length })
    return if (length == 4) "OK" else "FAIL: $length"
}
