// LANGUAGE: +CollectionLiterals
// An indexed store's value is a collection literal: `set` is selected with the literal admitted by
// its array parameter, and the literal is then built for that parameter.

class Arrays {
    var last: IntArray = IntArray(0)
    operator fun set(index: Int, value: IntArray) {
        last = value
    }
}

fun box(): String {
    val nested = arrayOf(IntArray(0))
    nested[0] = [1, 2]
    if (nested[0].size != 2 || nested[0][1] != 2) return "array: ${nested[0].size}"
    val arrays = Arrays()
    arrays[0] = [3, 4, 5]
    if (arrays.last.size != 3) return "set: ${arrays.last.size}"
    return "OK"
}
