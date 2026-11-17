// An anonymous object built in a property initializer of another one reads the value both capture
// from the outer object's constructor parameter, so the outer object keeps no field for it.
interface HasValue {
    val value: Any
}

fun wrap(text: () -> String): HasValue =
    object : HasValue {
        override val value = object {
            override fun toString() = text()
        }
    }

fun box(): String = wrap { "O" }.value.toString() + wrap { "K" }.value.toString()
