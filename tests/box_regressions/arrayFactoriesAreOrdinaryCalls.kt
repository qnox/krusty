// The `kotlin` array factory functions are ordinary generic declarations
// (`fun <reified T> arrayOf(vararg elements: T): Array<T>`), so a call infers and checks them
// like any other call: from the arguments, from a spread `Array<out T>`, or from the parameter
// that receives the result when the call itself has no element evidence.

interface Item { val name: String }
class Named(override val name: String) : Item

fun names(values: Array<out Item>): String {
    var result = ""
    for (value in values) result += value.name
    return result
}

open class Holder(val label: String, vararg val items: Item) {
    fun render(): String = label + names(items)
}

class Spread : Holder("S", *arrayOf())
class Secondary : Holder {
    constructor() : super("C", *emptyArray())
}

fun <T> sizeOf(values: Array<T>): Int = values.size

fun projected(flag: Boolean): Array<out Any> =
    if (flag) arrayOf<String>("a", "b") else arrayOf<Int>(1)

fun box(): String {
    if (Spread().render() != "S") return "empty spread"
    if (Secondary().render() != "C") return "secondary"

    val strings: Array<String> = arrayOf()
    if (strings.size != 0) return "contextual empty"
    if (sizeOf<String>(arrayOf()) != 0) return "argument empty"
    if (names(arrayOf(Named("O"), Named("K"))) != "OK") return "projected parameter"

    val spread = arrayOf(*projected(true), *projected(false), "c")
    if (spread.size != 4 || spread[2] != 1 || spread[3] != "c") return "projected spread"

    val ints = intArrayOf(1, *intArrayOf(2, 3))
    if (ints.size != 3 || ints[0] + ints[1] + ints[2] != 6) return "primitive spread"
    val nulls = arrayOfNulls<String>(2)
    if (nulls.size != 2 || nulls[0] != null) return "nulls"
    val qualified = kotlin.arrayOf("O", "K")
    return qualified[0] + qualified[1]
}
