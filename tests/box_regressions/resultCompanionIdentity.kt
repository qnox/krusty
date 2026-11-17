fun box(): String {
    val first: Any? = Result.Companion
    val second: Any = Result.Companion
    if (first == null) return "null companion"
    if (first !== second || first != second) return "unstable identity"
    if (first === Unit || first === Int.Companion) return "shared identity"
    if (first !is Result.Companion || first is Int.Companion) return "wrong classifier"
    val checked = first as Result.Companion
    if (checked !== second) return "explicit cast"
    if ((Any() as? Result.Companion) != null) return "wrong Any cast"
    if (first.hashCode() != second.hashCode()) return "unstable hash"
    if (first.toString() != second.toString()) return "unstable rendering"
    val success = Result.success(42)
    if (success.getOrNull() != 42) return "success"
    val failure = IllegalStateException("expected")
    if (Result.failure<Int>(failure).exceptionOrNull() !== failure) return "failure"
    return "OK"
}
