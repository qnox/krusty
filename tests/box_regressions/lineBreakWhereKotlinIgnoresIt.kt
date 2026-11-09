// kotlinc's parser treats a line break as whitespace except at the few points that consult it.
// Every construct here continues across a line break.
open class Base(val value: String)

class
Named(value: String) : Base(value)

interface Source {
    fun text(): String
}

class Fixed(private val text: String) : Source {
    override fun text() = text
}

class Delegated(source: Source)
    : Source
    by source

fun <T>
    identity(value: T): T = value

fun returnType(value: String)
    : String = value

fun box(): String {
    val named = Named(identity("O")).
        value
    val result = named + returnType("K")
    val delegated = Delegated(Fixed("OK")).text()
    return if (result == "OK" && delegated == "OK") "OK" else "FAIL: $result $delegated"
}
