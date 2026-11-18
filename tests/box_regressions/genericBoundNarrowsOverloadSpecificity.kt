class Col<T>(val tag: String)

class Id<T : Any>(val value: T)

class Holder {
    fun <T : Any> reference(name: String, column: Col<T>, onDelete: Int? = null): Col<T> = Col("plain:" + name)

    fun <T : Any, E : Id<T>> reference(name: String, column: Col<E>, onDelete: Int? = null): String = "id:" + name

    fun <T : Any, E : Id<T>> optional(name: String, column: Col<E>): String =
        reference(name, column, 1)

    fun plain(name: String, column: Col<String>): String = reference(name, column).tag
}

fun box(): String {
    val holder = Holder()
    if (holder.optional("a", Col<Id<Int>>("c")) != "id:a") return "bounded"
    if (holder.plain("b", Col<String>("c")) != "plain:b") return "unbounded"
    return "OK"
}
