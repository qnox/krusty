// A bare call of a function-typed property the extension receiver supplies types its trailing lambda
// against the property's parameter, including an extension-function-typed one.

class Subscribers(val subscribe: (String, Int.(String) -> String) -> String)

fun Subscribers.content(handler: Int.(String) -> String): String =
    subscribe("content") { handler(this, it) }

fun Subscribers.plain(): String = subscribe("plain") { "$this:$it" }

class Shadowed(val subscribe: (String) -> String) {
    fun subscribe(text: String, block: (String) -> String): String = block(text) + "!"
}

fun Shadowed.call(): String = subscribe("x") { it + "?" }

fun box(): String {
    val subscribers = Subscribers { name, block -> name + block(7, "a") }
    if (subscribers.content { "$this$it" } != "content7a") return "content: ${subscribers.content { "$this$it" }}"
    if (subscribers.plain() != "plain7:a") return "plain: ${subscribers.plain()}"
    if (Shadowed { it }.call() != "x?!") return "shadowed: ${Shadowed { it }.call()}"
    return "OK"
}
