// An enum's primary constructor may carry annotations and modifiers before `constructor`. Each
// form parses, and its entries are constructed through it as usual.

annotation class Mark

enum class Private private constructor(val code: Int) {
    A(1),
    B(2),
}

enum class Annotated @Mark constructor(val label: String, val weight: Int = 10) {
    X("x"),
    Y("y", 20),
}

enum class Both @Mark private constructor(val n: Int) {
    ONE(1),
    TWO(2);

    fun twice() = n + n
}

fun box(): String {
    if (Private.A.code + Private.B.code != 3) return "Fail private: ${Private.entries}"
    if (Annotated.X.label + Annotated.X.weight != "x10") return "Fail default: ${Annotated.X.weight}"
    if (Annotated.Y.label + Annotated.Y.weight != "y20") return "Fail explicit: ${Annotated.Y.weight}"
    if (Both.ONE.twice() + Both.TWO.twice() != 6) return "Fail member: ${Both.ONE.twice()}"
    if (Both.valueOf("TWO").ordinal != 1) return "Fail ordinal"
    return "OK"
}
