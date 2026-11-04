open class MixedConstructorParent(val number: Int)

private fun mixedInput(text: String, number: Int): String {
    class Local : MixedConstructorParent(number) {
        fun read(): String = text + this.number.toString()
    }
    return Local().read()
}

fun box(): String = if (mixedInput("O", 7) == "O7") "OK" else "mixed-captures"
