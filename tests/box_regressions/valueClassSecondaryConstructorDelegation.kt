// A value class's secondary constructor delegates to another constructor with arguments adapted to
// that constructor's declared parameters: a value-class parameter takes the carrier and an `Any`
// parameter takes the box.
OPTIONAL_JVM_INLINE_ANNOTATION
value class Tag(val value: Int)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Holder(val text: String) {
    constructor(tag: Tag) : this("tag" + tag.value)
    constructor(item: Any, tag: Tag) : this(item.toString() + tag.value) {
        check(text.length > 0)
    }
}

fun box(): String {
    val fromTag = Holder(Tag(3))
    if (fromTag.text != "tag3") return "fromTag: ${fromTag.text}"
    val fromAny = Holder(Tag(4), Tag(5))
    if (fromAny.text != "Tag(value=4)5") return "fromAny: ${fromAny.text}"
    return "OK"
}
