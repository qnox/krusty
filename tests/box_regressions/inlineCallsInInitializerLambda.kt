fun invokeBlock(block: () -> Unit) {
    block()
}

inline fun append(values: MutableCollection<String>, value: String) {
    values.add(value)
}

class InitializedValues {
    val values = arrayListOf("O")

    init {
        values += "A"
        invokeBlock {
            values += "B"
            append(values, "C")
        }
        fun named() {
            invokeBlock {
                values += "D"
                append(values, "E")
            }
        }
        named()
    }
}

fun box(): String {
    val values = InitializedValues().values
    return if (values == listOf("O", "A", "B", "C", "D", "E")) "OK" else "contents"
}
