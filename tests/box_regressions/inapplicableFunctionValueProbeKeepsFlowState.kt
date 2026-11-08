class Receiver {
    fun block(count: Int, text: String): String = "$count$text"
}

fun pick(value: String?): Int = 1

fun pick(value: String): String = "picked"

fun call(value: String?, block: (Int, Int) -> String): String =
    with(Receiver()) { block(pick(value), value!!) }

fun box(): String {
    val result = call("K") { _, _ -> "local" }
    return if (result == "1K") "OK" else result
}
