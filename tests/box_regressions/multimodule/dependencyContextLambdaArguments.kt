// WITH_STDLIB
// A dependency's context-parameter function type keeps its context count, so the lambda binds
// both context arguments and its extension receiver. A trailing lambda after an omitted defaulted
// fun-interface parameter converts to its own function type, not to that interface (kotlinc 2.4.21).
// MODULE: lib
// FILE: Library.kt
package fixture

class Route {
    var out = ""
}

class PrincipalContext<P>(val principal: P)

class Scheme<P, C>(val principal: P, val context: C)

fun interface Handler {
    fun Route.onFail(cause: String)
}

fun Route.plainWith(
    scheme: Scheme<String, Int>,
    build: context(PrincipalContext<String>, Int) Route.() -> Unit,
): Route {
    with(PrincipalContext(scheme.principal)) { with(scheme.context) { build() } }
    return this
}

fun <P : Any, C> Route.authWith(
    scheme: Scheme<P, C>,
    onFail: Handler? = null,
    build: context(PrincipalContext<P>, C) Route.() -> Unit,
): Route {
    with(PrincipalContext(scheme.principal)) { with(scheme.context) { build() } }
    if (onFail != null) with(onFail) { this@authWith.onFail("!") }
    return this
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

context(principal: PrincipalContext<String>)
val name: String
    get() = principal.principal

context(count: Int)
val repeated: String
    get() = if (count == 1) "K" else "?"

fun box(): String {
    val plain = Route().plainWith(Scheme("O", 1)) { out = name + repeated }
    if (plain.out != "OK") return "FAIL plain: " + plain.out
    val defaulted = Route().authWith(Scheme("O", 1)) { out = name + repeated }
    if (defaulted.out != "OK") return "FAIL defaulted: " + defaulted.out
    return "OK"
}
