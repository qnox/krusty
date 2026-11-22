// WITH_STDLIB

fun <T, V> ((T) -> V).noArgument(value: V): T? = null

inline fun <reified T : Enum<T>, V> ((T) -> V).find(value: V): T? {
    return enumValues<T>().firstOrNull { this(it) == value }
}

// `enumValues` of a reified parameter stays a template until the outermost call names the enum.
inline fun <reified T : Enum<T>> countOf(): Int = enumValues<T>().size
inline fun <reified E : Enum<E>> twiceCountOf(): Int = countOf<E>() * 2

class Box(val text: String) {
    fun label() = text
    fun shout() = text + "!"
}

enum class Animal(val string: String) {
    Cat("cat"),
    Dog("dog");

    fun label() = string
    fun code() = "$string!"
}

// A branch typed `Nothing` gives the conditional's join no evidence, so each inferred result is the
// other branch's own type: the receiver bounds `T`, and the `Nothing` sibling must not cap it.
fun enumFunction(label: String) = Animal::label.find(label) ?: throw IllegalArgumentException(label)
fun enumCode(code: String) = Animal::code.find(code) ?: throw IllegalArgumentException(code)
fun ifBranch(fail: Boolean) =
    if (fail) throw IllegalStateException("fail") else Box::label.noArgument("x")
fun whenBranch(mode: Int) = when (mode) {
    0 -> throw IllegalStateException("zero")
    1 -> Box::shout.noArgument("x")
    else -> throw IllegalStateException("other")
}

fun box(): String {
    val dog: Animal = enumFunction("dog")
    val cat: Animal = enumCode("cat!")
    val fromIf: Box? = ifBranch(false)
    val fromWhen: Box? = whenBranch(1)
    if (dog != Animal.Dog || cat != Animal.Cat) return "Fail: enums"
    if (countOf<Animal>() != 2 || twiceCountOf<Animal>() != 4) return "Fail: enum counts"
    if (fromIf != null || fromWhen != null) return "Fail: conditionals"
    try {
        enumCode("cow!")
        return "Fail: no exception"
    } catch (e: IllegalArgumentException) {
        if (e.message != "cow!") return "Fail: ${e.message}"
    }
    return "OK"
}
