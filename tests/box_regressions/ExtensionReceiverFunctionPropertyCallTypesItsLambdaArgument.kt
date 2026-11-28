// A bare call of a function-typed property the extension receiver supplies types its trailing lambda
// against the property's parameter, including an extension-function-typed one. A same-named member
// function the call can apply to is the nearer callable; one it cannot apply to leaves the property's
// invoke as the candidate of that receiver. A `with` receiver, or a member extension's extension
// receiver, is nearer than the dispatch receiver, so its property types the lambda over the class's
// same-named one. With no nearer property, the class's own property types a call's arguments, a
// lambda or not, over a same-named top-level function.

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

class Texts(val subscribe: (String, (String) -> String) -> String)

class Numbers(val subscribe: (String, (Int) -> String) -> String) {
    fun Texts.call(): String = subscribe("t") { it.length.toString() + it }
    fun viaWith(texts: Texts): String = with(texts) { subscribe("w") { it.length.toString() + it } }
}

class Box<T>(val content: T? = null)

fun reference(count: Int): String = "top-level $count"

class Holder(val reference: (Box<String>) -> String) {
    fun call(): String = reference(Box())
}

fun box(): String {
    val subscribers = Subscribers { name, block -> name + block(7, "a") }
    if (subscribers.content { "$this$it" } != "content7a") return "content: ${subscribers.content { "$this$it" }}"
    if (subscribers.plain() != "plain7:a") return "plain: ${subscribers.plain()}"
    if (Shadowed { it }.call() != "x?!") return "shadowed: ${Shadowed { it }.call()}"
    val inapplicable = Inapplicable { text, block -> text + block(41) }
    if (inapplicable.call() != "y42") return "inapplicable: ${inapplicable.call()}"
    if (inapplicable.subscribe("z") != "z!") return "member: ${inapplicable.subscribe("z")}"
    val texts = Texts { text, block -> block(text + "u") }
    val nearer = with(Numbers { text, block -> block(text.length) }) { texts.call() }
    if (nearer != "2tu") return "nearer: $nearer"
    val with = Numbers { text, block -> block(text.length) }.viaWith(texts)
    if (with != "2wu") return "with: $with"
    val held = Holder { box -> "held ${box.content ?: "empty"}" }.call()
    if (held != "held empty") return "held: $held"
    return "OK"
}
