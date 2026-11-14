open class Inv<T> {
    open fun tag(): String = "inv"
}

inline fun <reified T> make(): Inv<T> = object : Inv<T>() {
    override fun tag(): String = "OK"
}

fun box(): String = make<String>().tag()
