// A capturing closure that writes a local `var` keeps a smart cast on it stable when every value it
// writes is already of the smart cast's type: no interleaving of that write can break the proof.

open class Base {
    fun base(): Int = 1
}

class Derived : Base() {
    fun derived(): Int = 2
}

class Buffer {
    private val parts = StringBuilder()

    fun write(text: String) {
        parts.append(text)
    }

    fun read(): String = parts.toString()
}

// The local function writes only a non-null `Buffer`, and the enclosing function narrows the
// variable after declaring and calling it.
fun localFunctionWritesNonNull(): String {
    var buffer: Buffer? = null

    fun append(text: String) {
        if (buffer == null) {
            buffer = Buffer()
        }
        buffer.write(text)
    }

    append("O")
    append("K")
    if (buffer != null) return buffer.read()
    return "no buffer"
}

// A lambda created before the null check writes only non-null strings.
fun lambdaWritesNonNull(): Int {
    var text: String? = "abc"
    val reset = { text = "de" }
    if (text != null) {
        reset()
        return text.length
    }
    return -1
}

// A lambda created inside the proven branch writes only non-null strings.
fun lambdaInsideProofWritesNonNull(): Int {
    var text: String? = "abc"
    if (text != null) {
        val reset = { text = "de" }
        reset()
        return text.length
    }
    return -1
}

// A type test survives a write of a subtype of the tested type.
fun lambdaWritesSubtype(): Int {
    var value: Base = Derived()
    val replace = { value = Derived() }
    if (value is Derived) {
        replace()
        return value.derived()
    }
    return value.base()
}

// A type test survives a write of exactly the tested type into a wider declaration.
fun lambdaWritesTestedType(): Int {
    var chars: CharSequence? = "abc"
    val replace = { chars = "wxyz" }
    if (chars is String) {
        replace()
        return chars.length
    }
    return -1
}

// A straight-line assignment narrows even though a lambda writes the variable, because the lambda
// writes only values of the assigned type.
fun assignmentAfterLambdaWritesNonNull(): Int {
    var text: String? = null
    val reset = { text = "de" }
    text = "abc"
    return text.length
}

fun box(): String {
    val buffered = localFunctionWritesNonNull()
    if (buffered != "OK") return "local function: $buffered"
    if (lambdaWritesNonNull() != 2) return "lambda: ${lambdaWritesNonNull()}"
    if (lambdaInsideProofWritesNonNull() != 2) return "lambda inside proof"
    if (lambdaWritesSubtype() != 2) return "subtype: ${lambdaWritesSubtype()}"
    if (lambdaWritesTestedType() != 4) return "tested type: ${lambdaWritesTestedType()}"
    if (assignmentAfterLambdaWritesNonNull() != 3) return "assignment"
    return "OK"
}
