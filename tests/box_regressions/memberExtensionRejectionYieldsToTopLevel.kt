// A member extension that rejects the call's arguments does not end resolution: like any
// inapplicable tower level, it gives way to an applicable top-level extension on the same receiver.
class Builder(val name: String) {
    fun String.tag(separator: String): String = this + separator
    fun render(item: String): String = item.tag(this)
    fun renderOwn(item: String): String = item.tag("!")
}
fun String.tag(builder: Builder): String = builder.name + ":" + this
fun box(): String {
    val builder = Builder("b")
    if (builder.render("a") != "b:a") return "fail top-level"
    if (builder.renderOwn("a") != "a!") return "fail member"
    return "OK"
}
