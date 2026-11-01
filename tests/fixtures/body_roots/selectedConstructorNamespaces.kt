fun selectedInputs(first: String, second: String): String {
    val owner = object {
        fun fromFirst(): String {
            class Reader { fun read(): String = first }
            return object {
                fun value(): String = second + Reader().read()
            }.value()
        }
        fun fromSecond(): String {
            class Reader { fun read(): String = second }
            return object {
                fun value(): String = first + Reader().read()
            }.value()
        }
    }
    return owner.fromFirst() + owner.fromSecond()
}

fun box(): String {
    val result = selectedInputs("O", "K")
    return if (result == "KOOK") "OK" else result
}
