// A bare call of a function-typed property the extension receiver supplies types its trailing lambda
// against the property's parameter, including an extension-function-typed one. A same-named member
// function the call can apply to is the nearer callable; one it cannot apply to leaves the property's
// invoke as the candidate of that receiver.

class Subscribers(val subscribe: (String, Int.(String) -> String) -> String)

fun Subscribers.content(handler: Int.(String) -> String): String =
    subscribe("content") { handler(this, it) }

fun Subscribers.plain(): String = subscribe("plain") { "$this:$it" }

class Shadowed(val subscribe: (String) -> String) {
    fun subscribe(text: String, block: (String) -> String): String = block(text) + "!"
}

fun Shadowed.call(): String = subscribe("x") { it + "?" }

class Inapplicable(val subscribe: (String, (Int) -> String) -> String) {
    fun subscribe(text: String): String = text + "!"
}

fun Inapplicable.call(): String = subscribe("y") { (it + 1).toString() }

fun box(): String {
    val subscribers = Subscribers { name, block -> name + block(7, "a") }
    if (subscribers.content { "$this$it" } != "content7a") return "content: ${subscribers.content { "$this$it" }}"
    if (subscribers.plain() != "plain7:a") return "plain: ${subscribers.plain()}"
    if (Shadowed { it }.call() != "x?!") return "shadowed: ${Shadowed { it }.call()}"
    val inapplicable = Inapplicable { text, block -> text + block(41) }
    if (inapplicable.call() != "y42") return "inapplicable: ${inapplicable.call()}"
    if (inapplicable.subscribe("z") != "z!") return "member: ${inapplicable.subscribe("z")}"
    return "OK"
}
