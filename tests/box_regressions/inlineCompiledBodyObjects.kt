// FILE: Declaration.kt
interface Named {
    fun name(): String
}

inline fun named(): Named = object : Named {
    override fun name() = "O"
}

inline fun counted(times: Int): Int {
    val counter = object {
        var count = 0
    }
    repeat(times) { counter.count++ }
    return counter.count
}

class Box<T>

private inline fun <reified T> Box<T>.tagged(): Named = object : Named {
    override fun name() = T::class.simpleName!!
}

fun tags(): String = Box<String>().tagged().name() + Box<Int>().tagged().name()
// FILE: Caller.kt
fun box(): String {
    val first = named()
    val second = named()
    if (counted(3) != 3) return "counted"
    if (tags() != "StringInt") return "tags ${tags()}"
    return if (second.name() == "O") first.name() + "K" else "second"
}
