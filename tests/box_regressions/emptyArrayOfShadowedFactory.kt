// A declared zero-argument `arrayOf` shadows the stdlib factory, so the argument is an ordinary
// call and is never postponed as an array factory.
open class Base(val value: String)

fun arrayOf(): String = "shadow"

class Header : Base(arrayOf())

class Secondary : Base {
    constructor() : super(arrayOf())
}

fun box(): String {
    if (Header().value != "shadow") return "Fail header"
    if (Secondary().value != "shadow") return "Fail secondary"
    if (Base(arrayOf()).value != "shadow") return "Fail call"
    return "OK"
}
