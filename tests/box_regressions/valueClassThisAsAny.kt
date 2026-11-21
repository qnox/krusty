// A value class's `this` is its carrier inside the member, so returning it where the member
// declares a reference result (`Any`, a type parameter, a supertype) boxes it, whether the member is
// a function or a computed property accessor.
interface Named {
    fun name(): String
}

OPTIONAL_JVM_INLINE_ANNOTATION
value class Tag(val value: Int) : Named {
    override fun name(): String = "tag" + value
    fun asAny(): Any = this
    fun asNamed(): Named = this
    val selfAsAny: Any get() = this
    val selfAsNamed: Named get() = this
}

fun box(): String {
    val tag = Tag(3)
    if (tag.asAny() != tag) return "fail function Any"
    if (tag.asNamed().name() != "tag3") return "fail function Named"
    if (tag.selfAsAny != tag) return "fail property Any"
    if (tag.selfAsNamed.name() != "tag3") return "fail property Named"
    if (tag.selfAsAny !is Tag) return "fail property identity"
    return "OK"
}
