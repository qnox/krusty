// WITH_STDLIB

external interface Handle : JsAny

fun describe(value: JsAny?): String = if (value == null) "none" else "some"

fun box(): String {
    val none: JsAny? = null
    if (describe(none) != "none") return "FAIL null JsAny"
    return "OK"
}
