fun <T> measure(size: (T) -> Int, make: () -> T): Int = size(make())

fun relay() = measure({ it.length }, { "four" })

fun box(): String = if (relay() == 4) "OK" else "FAIL"
