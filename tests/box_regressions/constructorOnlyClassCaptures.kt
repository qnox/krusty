private var constructions = 0

open class ConstructorValueParent(val text: String, val number: Int) {
    init { constructions++ }
}

interface ConstructorValueReading { fun read(): String }

class ConstructorValueReader(private val text: String, private val number: Int) : ConstructorValueReading {
    init { constructions++ }
    override fun read(): String = if (number == 9) text else "delegate-number"
}

private fun superclassInput(value: String, number: Int): String {
    class Local : ConstructorValueParent(value, number)
    val result = Local()
    return if (result.number == 7) result.text else "super-number"
}

private fun delegateInput(value: String, number: Int): String {
    class Local : ConstructorValueReading by ConstructorValueReader(value, number)
    return Local().read()
}

fun box(): String {
    constructions = 0
    val result = superclassInput("O", 7) + delegateInput("K", 9)
    return if (constructions == 2) result else "construction-count:$constructions"
}
