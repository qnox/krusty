// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: the declarations under test are Java library members (`java.util`, `java.lang`).
// A value-class value handed to a Java member's `Object` parameter reaches it boxed: the selected
// Java declaration's parameter types decide the representation, as they do for a Kotlin member.
OPTIONAL_JVM_INLINE_ANNOTATION
value class Tag(val value: Int)

fun box(): String {
    val list = java.util.ArrayList<Any>()
    list.add(Tag(3))
    if (list.size != 1) return "fail size"
    val first = list[0]
    if (first !is Tag) return "fail element is not the box: $first"
    if (first.value != 3) return "fail element value"
    val sb = StringBuilder()
    sb.append(Tag(4))
    if (sb.toString() != "Tag(value=4)") return "fail append: $sb"
    return "OK"
}
