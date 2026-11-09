package fixture

interface First {
    fun first(): String
}

interface Second<T> {
    fun second(value: T): CharSequence
    val carried: T
}

inline fun make(value: String): CharSequence {
    val obj = object : First, Second<String> {
        override fun first(): String = value
        override fun second(value: String): String = value
        override val carried: String
            get() = value
    }
    return (obj as Second<String>).second(value)
}
