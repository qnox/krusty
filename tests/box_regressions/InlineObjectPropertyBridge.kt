// KOTLINC_VERSION: [2.5.0,)
// kotlinc before 2.5 miscompiles this program (KT-20245), so it cannot be the oracle.
interface HasValue {
    val value: Any
}

inline fun wrap(crossinline text: () -> String) =
    object : HasValue {
        override val value = object {
            override fun toString() = text()
        }
    }

fun read(holder: HasValue): String = holder.value.toString()

fun box(): String = read(wrap { "O" }) + read(wrap { "K" })
