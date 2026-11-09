// LANGUAGE: +CollectionLiterals
// An indexed store's value is a collection literal built by the parameter classifier's companion
// `operator fun of`: `set` is selected with the literal admitted by that factory.

class Bag(val size: Int) {
    companion object {
        operator fun of(vararg values: String) = Bag(values.size)
    }
}

class Bags {
    var last: Bag = Bag(0)
    operator fun set(index: Int, value: Bag) {
        last = value
    }
}

fun box(): String {
    val bags = Bags()
    bags[0] = ["a", "b"]
    if (bags.last.size != 2) return "of: ${bags.last.size}"
    return "OK"
}
