// WITH_STDLIB

fun <T : Enum<T>, V> ((T) -> V).find(value: V, candidates: Array<T>): T? {
    return candidates.firstOrNull { this(it) == value }
}

fun <T, V> ((T) -> V).argumentOf(value: V): T? = null

class Box(val text: String) {
    fun size() = text.length
}

enum class Animal(val label: String) {
    Cat("cat"),
    Dog("dog");

    fun name2() = label
}

fun fromLabel(label: String) = Animal::name2.find(label, Animal.values())
    ?: throw IllegalArgumentException("Bad animal: '$label'")

val reference: kotlin.reflect.KFunction1<Box, Int> = Box::size

fun referenceReceiver() = Box::size.argumentOf(1)
fun valueReceiver() = reference.argumentOf(1)

fun box(): String {
    val direct: Box? = referenceReceiver()
    val fromValue: Box? = valueReceiver()
    if (direct != null || fromValue != null) return "Fail: null expected"
    if (fromLabel("dog") != Animal.Dog) return "Fail: dog"
    try {
        fromLabel("cow")
        return "Fail: no exception"
    } catch (e: IllegalArgumentException) {
        if (e.message != "Bad animal: 'cow'") return "Fail: ${e.message}"
    }
    return "OK"
}
