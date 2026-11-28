// WITH_STDLIB
// A member extension whose return is inferred from its body reads its implicit extension receiver
// with the receiver's own type variable, so `add(el)` maps `el: T` to `ArrayList<T>.add(T)` even
// when the Java parameter is enhanced from the overridden `MutableList.add(E)`.

class Sink {
    fun <T> ArrayList<T>.push(el: T) = add(el)

    val <T> ArrayList<T>.pushFirst get() = add(get(0))

    fun fill(list: ArrayList<String?>): Boolean = list.push("O") && list.push(null) && list.pushFirst
}

class Typed<T> {
    fun ArrayList<T>.push(el: T) = add(el)

    fun fill(list: ArrayList<T>, el: T): Boolean = list.push(el)
}

fun box(): String {
    val list = ArrayList<String?>()
    if (!Sink().fill(list)) return "fail: fill"
    val typed = ArrayList<String>()
    if (!Typed<String>().fill(typed, "K")) return "fail: typed"
    return if (list == listOf("O", null, "O")) list[0] + typed[0] else "fail: $list"
}
