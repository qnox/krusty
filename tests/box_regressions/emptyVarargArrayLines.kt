// An omitted vararg and an argument-less `arrayOf()` pass an array kotlinc builds without a source
// position, so a call they start marks its line only at its own invoke. `emptyArray()` is a call
// of its own and marks its line where its array is built.
fun count(vararg values: Int): Int = values.size

fun names(vararg values: String): Int = values.size

fun omitted(): Int = count()

fun omittedReference(): Int {
    return names()
}

fun omittedStatement() {
    count()
}

fun emptyFactory(): Int = names(*arrayOf())

fun emptyArrayCall(values: Array<String>): Int = values.size

fun box(): String {
    omittedStatement()
    if (omitted() != 0) return "omitted"
    if (omittedReference() != 0) return "omittedReference"
    if (emptyFactory() != 0) return "emptyFactory"
    if (emptyArrayCall(emptyArray()) != 0) return "emptyArray"
    if (emptyArrayCall(arrayOf()) != 0) return "arrayOf"
    return "OK"
}
