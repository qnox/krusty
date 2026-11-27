// An anonymous object inherits an interface default its body never calls. The library calls the
// member through the interface the default implements, so only dispatch reaches the default, and
// the default's own call reaches `suffix`. `Chain.next` constructs an object inheriting `next`
// itself.
// MODULE: lib
package fixture

interface Keyed {
    fun key(): String
}

interface Named : Keyed {
    override fun key(): String = "O" + suffix()
}

fun suffix(): String = "K"

fun keyOf(keyed: Keyed): String = keyed.key()

fun named(): String = keyOf(object : Named {})

// A default that constructs an object inheriting that same default.
interface Chain {
    fun label(): String = "K"

    fun next(): Chain = object : Chain {}
}

fun labelAfter(chain: Chain): String = chain.next().label()

fun chained(): String = labelAfter(object : Chain {})

// MODULE: main(lib)
package fixture

fun box(): String = if (named() == "OK" && chained() == "K") "OK" else "FAIL ${named()} ${chained()}"
