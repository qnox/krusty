// MODULE: lib
// A dependency class's constructor shapes a lambda passed for its fun-interface parameter by the
// interface method's inputs, also when another lambda argument comes first, positionally or named.
package lib

class Info(val text: String)

fun interface Provider {
    fun get(url: String): Info?
}

class Client(val agent: () -> String, val provider: Provider)

// MODULE: main(lib)
import lib.*

fun box(): String {
    val positional = Client({ "a" }, { url -> Info(url + "1") })
    if (positional.provider.get("x")?.text != "x1") return "positional"
    val named = Client(agent = { "b" }, provider = { url -> Info(url + "K") })
    if (named.provider.get("O")?.text != "OK") return "named"
    val reordered = Client(provider = { url -> Info(url + "!") }, agent = { "c" })
    if (reordered.provider.get("r")?.text != "r!") return "reordered"
    return "OK"
}
