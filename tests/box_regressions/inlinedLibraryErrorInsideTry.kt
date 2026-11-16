inline fun guarded(block: () -> Int): Int {
    return block()
}

fun box(): String {
    val failed = try { guarded { error("x") } } catch (_: IllegalStateException) { -1 }
    if (failed != -1) return "fail: $failed"
    return "OK"
}
