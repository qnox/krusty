// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual
// WITH_STDLIB
// A contextual reference without a written type: the declaration's type is inferred with the
// context parameters bound, and a local class captures the context values the reference binds.

var storage = ""

context(c: Holder)
var prop: String
    get() = storage + c.tag
    set(value) {
        storage = value + c.tag
    }

context(c: Holder)
fun describe(): String = "d" + c.tag

context(s: String)
fun suffixed(value: String): String = value + s

context(n: Int, s: String)
val pair: String
    get() = s + n

class Holder(val tag: String) {
    var delegated by ::prop
    val function = ::describe
}

fun box(): String {
    val a = Holder("A")
    a.delegated = "O"
    if (storage != "OA") return "FAIL 1: $storage"
    if (a.delegated != "OAA") return "FAIL 2: ${a.delegated}"
    if (a.function() != "dA") return "FAIL 3: ${a.function()}"

    val local = context(1, "K") {
        class Local {
            val delegated by ::pair
            val function = ::suffixed
        }
        val instance = Local()
        instance.delegated + instance.function("x")
    }
    if (local != "K1xK") return "FAIL 4: $local"
    return "OK"
}
