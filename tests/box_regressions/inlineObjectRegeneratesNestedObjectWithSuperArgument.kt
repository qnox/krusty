// KOTLINC_VERSION: [2.5.0,)
// kotlinc before 2.5 miscompiles this program (KT-20245), so it cannot be the oracle.
abstract class Base(val prefix: String) {
    abstract val value: Any
}

inline fun wrap(prefix: String, crossinline text: () -> String) =
    object : Base(prefix) {
        override val value = object {
            override fun toString() = text()
        }
    }

fun read(base: Base): String = base.prefix + base.value.toString()

fun box(): String {
    val first = read(wrap("O") { "K" })
    val second = read(wrap("") { "" })
    return if (second.isEmpty()) first else "fail: $second"
}
