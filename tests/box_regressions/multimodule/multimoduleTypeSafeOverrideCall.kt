// MODULE: lib
class Values(private val source: Map<String, Int>) : Map<String, Int> by source
abstract class Lengths : AbstractMap<String, Int>() {
    override val entries: Set<Map.Entry<String, Int>> get() = throw UnsupportedOperationException()
    override fun get(key: String): Int? = key.length
    override fun containsKey(key: String): Boolean = key.isNotEmpty()
}
class Words : Lengths()
// MODULE: main(lib)
fun read(values: Values): Int? = values["one"]
fun length(words: Words): Int? = words["four"]
fun has(words: Lengths): Boolean = words.containsKey("k")
fun box(): String {
    if (read(Values(mapOf("one" to 1))) != 1) return "delegated get"
    if (length(Words()) != 4) return "overridden get"
    if (!has(Words()) || (Words() as Map<Any, Any>).containsKey(1)) return "containsKey"
    return "OK"
}
