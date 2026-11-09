// An empty `arrayOf()` stays postponed while constructor overloads are compared, so only the
// overload with an array parameter applies, in a delegation header, a `super` call, and a plain
// constructor call alike.
open class Pick(val kind: String) {
    constructor(values: Array<String>) : this("array${values.size}")
}

class FromHeader : Pick(arrayOf())

class FromSuper : Pick {
    constructor() : super(arrayOf())
}

fun pick(value: String) = "string"

fun pick(values: Array<String>) = "array${values.size}"

fun box(): String {
    if (FromHeader().kind != "array0") return "Fail header ${FromHeader().kind}"
    if (FromSuper().kind != "array0") return "Fail super ${FromSuper().kind}"
    if (Pick(arrayOf()).kind != "array0") return "Fail call"
    if (pick(arrayOf()) != "array0") return "Fail function"
    return "OK"
}
