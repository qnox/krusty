package fixture

interface Probe {
    fun label(): String
    fun accepts(value: Any?): Boolean
    fun run(): Int
}

inline fun <reified T> probe(label: String, crossinline action: () -> Int): Probe =
    object : Probe {
        override fun label(): String = label
        override fun accepts(value: Any?): Boolean = value is T
        override fun run(): Int = action()
    }
