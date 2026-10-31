// LANGUAGE: +ContextParameters
// MODULE: lib
private var evaluated = ""
fun evaluations(): String {
    val seen = evaluated
    evaluated = ""
    return seen
}
fun mark(step: String): String {
    evaluated += step
    return step
}
fun prefix(): String {
    mark("p")
    return "O"
}
fun answer(head: String = prefix(), tail: String = head + "!") = head + tail
fun String.twice(times: Int = length, separator: String = mark("-")) =
    if (times == 2) this + separator + this else this
context(s: String)
fun greet(useArg: Boolean, arg: String = mark("g") + s) = if (useArg) arg else s
context(s: String)
fun String.around(left: String = s, right: String = mark("r")) = left + this + right
// MODULE: main(lib)
fun box(): String {
    if (answer() != "OO!") return "fail: answer()"
    if (answer(tail = "K") != "OK") return "fail: answer(tail)"
    if (answer("K") != "KK!") return "fail: answer(head)"
    if (answer("O", "K") != "OK") return "fail: answer(head, tail)"
    // The callee evaluates a default once per call that omits it, after the supplied arguments.
    val answers = evaluations()
    if (answers != "pp") return "fail: prefix() evaluated $answers"
    if ("ab".twice() != "ab-ab") return "fail: twice()"
    if ("ab".twice(separator = mark("+")) != "ab+ab") return "fail: twice(separator)"
    if ("abc".twice() != "abc") return "fail: twice(3)"
    if ("ab".twice(2, "=") != "ab=ab") return "fail: twice(2, separator)"
    if (mark("a").twice(times = 1) != "a") return "fail: twice(times)"
    val separators = evaluations()
    if (separators != "-+-a-") return "fail: separator evaluated $separators"
    // A context operand is always supplied; only the value parameters after it default.
    val contextual = with("O") {
        greet(false) + greet(true) + greet(true, "K") + "x".around() + "y".around("<", ">")
    }
    if (contextual != "OgOKOxr<y>") return "fail: contextual $contextual"
    val contexts = evaluations()
    if (contexts != "ggr") return "fail: contextual defaults evaluated $contexts"
    return "OK"
}
