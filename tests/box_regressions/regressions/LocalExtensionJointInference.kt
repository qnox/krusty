class CovariantItems<out T>

fun box(): String {
    fun <T> CovariantItems<T>.pick(value: T): String = "OK"
    return CovariantItems<String>().pick(17)
}
