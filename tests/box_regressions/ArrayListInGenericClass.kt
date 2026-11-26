class Items<E> {
    val values = ArrayList<E>()

    fun add(item: E): Boolean = values.add(item)
}

fun <E> build(body: Items<E>.() -> Unit): List<E> {
    val items = Items<E>()
    items.body()
    return items.values
}

fun box(): String {
    val l = build<String> {
        add("O")
        add("K")
    }
    return l[0] + l[1]
}
