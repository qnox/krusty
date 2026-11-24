// WITH_STDLIB
OPTIONAL_JVM_INLINE_ANNOTATION
value class Plain(val v: Long)

fun <T> same(x: T): T = x

fun box(): String {
    if (Plain(1L) != Plain(1L)) return "fail direct equal"
    if (Plain(1L) == Plain(2L)) return "fail direct different"
    if (same(Plain(1L)) != same(Plain(1L))) return "fail erased both"
    if (same(Plain(1L)) != Plain(1L)) return "fail erased left"
    if (Plain(1L) != same(Plain(1L))) return "fail erased right"
    if (same(Plain(1L)) == same(Plain(2L))) return "fail erased different"
    return "OK"
}
