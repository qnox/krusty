package fixture

// An anonymous object inherits an interface default its body never calls. The library calls the
// member through the interface the default implements, so only dispatch reaches the default, and
// the default's own call reaches `suffix`.
interface Keyed {
    fun key(): String
}

interface Named : Keyed {
    override fun key(): String = "O" + suffix()
}

fun suffix(): String = "K"

fun keyOf(keyed: Keyed): String = keyed.key()

fun named(): String = keyOf(object : Named {})

fun box(): String = if (named() == "OK") "OK" else "FAIL ${named()}"
