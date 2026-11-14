// An empty `arrayOf()` passed through constructor delegation takes its element type from the
// selected parameter, exactly as it does in an ordinary constructor call. A named argument for the
// vararg parameter passes that array whole.
open class Expr<T>

open class Custom(val name: String, vararg val expr: Expr<*>) {
    constructor(prefix: Int) : this("p$prefix", *arrayOf())
}

object Named : Custom(name = "named", expr = arrayOf())

class Spread : Custom("spread", *arrayOf())

class Secondary : Custom {
    constructor() : super("secondary", *arrayOf())
    constructor(flag: Boolean) : super(name = "named$flag", expr = arrayOf())
}

val first = Expr<Int>()
val second = Expr<String>()

class Values : Custom(name = "values", expr = arrayOf(first, second))

class SecondaryValues : Custom {
    constructor() : super(name = "secondaryValues", expr = arrayOf(second, first))
}

val anonymous = object : Custom("anonymous", *arrayOf()) {}

fun box(): String {
    if (Named.name != "named") return "Fail named"
    if (Spread().name != "spread") return "Fail spread"
    if (Secondary().name != "secondary") return "Fail secondary"
    if (Secondary(true).name != "namedtrue") return "Fail secondary named"
    if (Custom(7).name != "p7") return "Fail this"
    if (anonymous.name != "anonymous") return "Fail anonymous"
    val values = Values().expr
    if (values[0] !== first || values[1] !== second) return "Fail values"
    val secondaryValues = SecondaryValues().expr
    if (secondaryValues[0] !== second || secondaryValues[1] !== first) return "Fail secondary values"
    return "OK"
}
