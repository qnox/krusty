// KOTLINC_VERSION: [2.5.0,)
// kotlinc before 2.5 miscompiles this program (KT-20245), so it cannot be the oracle.
inline fun wrap(crossinline text: () -> String) =
    object {
        val value = object {
            override fun toString() = text()
        }

        override fun toString() = value.toString()
    }

fun box(): String = wrap { "O" }.toString() + wrap { "K" }.toString()
