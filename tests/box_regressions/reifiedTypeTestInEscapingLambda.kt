// A reified inline function returns an escaping lambda that checks the call's type argument. The
// call site's copy of the lambda tests the bound type; the declaration's own lambda is a template.
interface Item
class Token : Item
class Other : Item

class Checks(val lambda: (Item) -> Boolean)

inline fun <reified T : Item> checks(): Checks = Checks({ it is T })

fun box(): String {
    val checks = checks<Token>()
    if (!checks.lambda(Token())) return "lambda rejects Token"
    if (checks.lambda(Other())) return "lambda accepts Other"
    return "OK"
}
