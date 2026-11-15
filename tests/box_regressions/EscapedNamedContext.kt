class EscapedCtx(val text: String)

context(c: EscapedCtx)
fun consumeEscapedContext(): String = c.text

context(`this`: EscapedCtx)
fun chooseEscapedContext(): String = consumeEscapedContext()

context(`this`: EscapedCtx)
fun String.chooseEscapedContextWithReceiver(): String = consumeEscapedContext()

class EscapedHost {
    context(`this`: EscapedCtx)
    fun chooseMemberContext(): String = consumeEscapedContext()
}

fun box(): String {
    val first = with(EscapedCtx("O")) { chooseEscapedContext() }
    val second = with(EscapedCtx("K")) { "unrelated".chooseEscapedContextWithReceiver() }
    val third = with(EscapedCtx("OK")) { EscapedHost().chooseMemberContext() }
    if (third != "OK") return "member:$third"
    return first + second
}
