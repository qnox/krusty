class NullableFunctionFields {
    val value: (() -> String)? = { "OK" }
    val missing: (() -> String)? = null
}

fun box(): String {
    val fields = NullableFunctionFields()
    if (fields.missing?.invoke() != null) return "missing"
    return fields.value?.invoke() ?: "value"
}
