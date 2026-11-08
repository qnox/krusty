// A member parameter `T & Any` read through a receiver whose argument is a type parameter (`this`
// inside `Conv<T>`) stays `T & Any`, so a reference to that member fits `(T & Any) -> R`.
interface Conv<T> {
    fun toDb(value: T?): Any? = value?.let(::notNull)

    fun reference(): (T & Any) -> Any = ::notNull

    fun notNull(value: T & Any): Any = value
}

class Strings : Conv<String>

class Wrapper<T>(private val conv: Conv<T>) {
    fun write(value: T & Any): Any = conv.notNull(value)
}

fun box(): String {
    val strings = Strings()
    if (strings.toDb("O") != "O") return "toDb"
    if (strings.toDb(null) != null) return "null"
    if (strings.reference()("K") != "K") return "reference"
    if (Wrapper(strings).write("OK") != "OK") return "wrapper"
    return "OK"
}
