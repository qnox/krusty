// A reified inline function returns an escaping anonymous object that checks the call's type
// argument. The call site's copy of the object tests the bound type; the declaration's own object
// is a template.
interface Item
class Token : Item
class Other : Item

interface Check {
    fun test(item: Item): Boolean
}

inline fun <reified T : Item> check(): Check =
    object : Check { override fun test(item: Item) = item is T }

fun box(): String {
    val check = check<Token>()
    if (!check.test(Token())) return "object rejects Token"
    if (check.test(Other())) return "object accepts Other"
    return "OK"
}
