// The `kotlin` array factory functions are ordinary generic declarations
// (`fun <reified T> arrayOf(vararg elements: T): Array<T>`), so a call infers and checks them
// like any other call: from its arguments, from the parameter that receives it, or from an
// explicit type argument. Spreads, primitive arrays, arrayOfNulls and empty factories completed by their expected type are
// covered by `array_factory_calls_e2e`.

interface Item { val name: String }
class Named(override val name: String) : Item

fun pair(values: Array<out Item>): String = values[0].name + values[1].name

fun box(): String {
    if (pair(arrayOf(Named("O"), Named("K"))) != "OK") return "projected parameter"
    val explicit = arrayOf<Item>(Named("K"))
    if (explicit[0].name != "K") return "explicit"
    val qualified = kotlin.arrayOf("O", "K")
    return qualified[0] + qualified[1]
}
