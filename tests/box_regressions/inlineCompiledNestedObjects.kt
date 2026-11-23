interface Named {
    fun name(): String
}

inline fun <reified T> make(): Named = object : Named {
    val type = T::class
    val nested: Named = object : Named {
        override fun name() = "nested"
    }

    override fun name() = nested.name()
}

fun box(): String = make<String>().name().let { if (it == "nested") "OK" else it }
