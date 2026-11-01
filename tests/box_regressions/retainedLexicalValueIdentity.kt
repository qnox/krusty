fun unrelated(first: String, second: String): String = first

fun box(): String {
    val captured = "OK"
    val reader = object { fun read(): String = captured }
    fun sameValue(): String = captured
    val observed = reader.read() + sameValue()
    return if (observed == "OKOK") "OK" else observed
}
