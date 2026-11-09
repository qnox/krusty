// `joinToString` appends each element through the stdlib's `appendElement`, which tests the
// element against `CharSequence?`; user code tests nullable targets the same way.

fun <T> describe(value: T): String =
    if (value is CharSequence?) "text:${value ?: "null"}" else "other"

fun isNotNullableString(value: Any?): Boolean = value !is String?

fun box(): String {
    val joined = listOf("a", "b", "c").joinToString(";")
    if (joined != "a;b;c") return "joined: $joined"
    val mixed = listOf<Any?>(1, null, 'x', "s").joinToString(",")
    if (mixed != "1,null,x,s") return "mixed: $mixed"
    if (describe("a") != "text:a") return "describe string"
    if (describe<String?>(null) != "text:null") return "describe null"
    if (describe(1) != "other") return "describe int"
    if (isNotNullableString(null)) return "null is String?"
    if (isNotNullableString("s")) return "\"s\" is String?"
    if (!isNotNullableString(1)) return "1 !is String?"
    return "OK"
}
