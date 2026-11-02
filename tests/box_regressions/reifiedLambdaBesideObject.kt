// One reified inline function creates both an escaping lambda and an escaping object that check
// the call's type argument. The expansion copies the object for the binding; the lambda's copy
// still checks the bound type.
interface Item
class Token : Item
class Other : Item

interface Check {
    fun test(item: Item): Boolean
}

class Checks(val lambda: (Item) -> Boolean, val check: Check)

inline fun <reified T : Item> checks(): Checks =
    Checks({ it is T }, object : Check { override fun test(item: Item) = item is T })

fun caller() = checks<Token>()

fun box(): String {
    val checks = caller()
    if (!checks.lambda(Token())) return "lambda rejects Token"
    if (checks.lambda(Other())) return "lambda accepts Other"
    if (!checks.check.test(Token())) return "object rejects Token"
    if (checks.check.test(Other())) return "object accepts Other"
    return "OK"
}
