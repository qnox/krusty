import kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED

fun suspended(): Any = COROUTINE_SUSPENDED

fun box(): String {
    val marker = COROUTINE_SUSPENDED
    if (marker !== COROUTINE_SUSPENDED) return "fail: two reads in one function"
    if (marker !== suspended()) return "fail: reads in two functions"
    return "OK"
}
