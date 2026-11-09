// WITH_STDLIB
// FILE: EnumEntryShadowedArrayFactory.kt

fun arrayOf(): String = "shadow"

enum class Kind(val value: String) { A(arrayOf()) }

fun box(): String = if (Kind.A.value == "shadow") "OK" else "Fail ${Kind.A.value}"
