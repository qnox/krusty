// FILE: main.kt
fun box(): String {
    val failed: Result<Int> = Result.failure(IllegalStateException("bad"))
    if (failed.getOrNull() != null) return "getOrNull"
    if (failureMessage(failed) != "bad") return "failure: ${failureMessage(failed)}"
    if (failureMessage(Result.success(1)) != "none") return "success"
    return "OK"
}

// FILE: failures.kt
fun failureMessage(result: Result<Int>): String = result.exceptionOrNull()?.message ?: "none"
