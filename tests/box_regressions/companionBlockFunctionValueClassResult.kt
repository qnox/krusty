// LANGUAGE: +CompanionBlocks
// A companion-block function that returns a value class hands its caller the class's value, the
// same as any other function returning it: the caller reads the result, stores it, passes it on
// and boxes it.
OPTIONAL_JVM_INLINE_ANNOTATION
value class W(val value: Int)

class C {
    companion {
        fun makes(): W = W(3)
        fun next(w: W): W = W(w.value + 1)
    }
}

fun box(): String {
    if (C.makes().value != 3) return "fail: read"
    val stored = C.makes()
    if (stored.value != 3) return "fail: stored"
    if (C.next(C.makes()).value != 4) return "fail: passed on"
    val boxed: Any = C.makes()
    if (boxed != W(3)) return "fail: boxed $boxed"
    return "OK"
}
