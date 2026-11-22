// A value-class value passed to a `super` call reaches the overridden member as its declaration
// takes it: boxed for an `Any` parameter and for a type parameter the supertype applies to the
// value class, and as the plain carrier for a value-class parameter.
OPTIONAL_JVM_INLINE_ANNOTATION
value class Tag(val value: Int)

open class Base {
    open fun describe(item: Any): String = item.toString()
    open fun twice(tag: Tag): Int = tag.value * 2
}

class Derived : Base() {
    override fun describe(item: Any): String = "<" + super.describe(Tag(3)) + ">"
    override fun twice(tag: Tag): Int = super.twice(Tag(tag.value + 1))
}

open class Holder<T> {
    open fun describe(item: T): String = (item as Any).toString()
}

class TagHolder : Holder<Tag>() {
    override fun describe(item: Tag): String = "[" + super.describe(Tag(item.value + 1)) + "]"
}

fun box(): String {
    val derived = Derived()
    if (derived.describe("ignored") != "<Tag(value=3)>") return "fail describe: ${derived.describe("ignored")}"
    if (derived.twice(Tag(4)) != 10) return "fail twice: ${derived.twice(Tag(4))}"
    val holder = TagHolder()
    if (holder.describe(Tag(5)) != "[Tag(value=6)]") return "fail generic: ${holder.describe(Tag(5))}"
    val erased: Holder<Tag> = holder
    if (erased.describe(Tag(7)) != "[Tag(value=8)]") return "fail bridge: ${erased.describe(Tag(7))}"
    return "OK"
}
