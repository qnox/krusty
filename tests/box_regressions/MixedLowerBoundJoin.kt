fun <T> any(vararg values: T): T = values[0]
fun <T : Number> first(vararg values: T): T = values[0]
fun box(): String {
    val joined = any(1, "a")
    val compared = joined.hashCode() == 1
    val sum = first(1, 4.5).toInt() + first(2, 3L).toInt()
    return if (compared && sum == 3) "OK" else "fail $compared $sum"
}
