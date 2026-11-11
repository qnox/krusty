// WITH_STDLIB

sealed interface ValueType {
    OPTIONAL_JVM_INLINE_ANNOTATION
    value class Number(val number: Int) : ValueType
    OPTIONAL_JVM_INLINE_ANNOTATION
    value class Other(val subtype: Subtype) : ValueType
}

interface Subtype {
    val id: Int
    OPTIONAL_JVM_INLINE_ANNOTATION
    value class Ref(override val id: Int) : Subtype
}

fun foo(f: Boolean): Int {
    val provider: () -> Subtype = { Subtype.Ref(1) }
    if (f)
        ValueType.Number(7)
    else
        ValueType.Other(provider())
    return 42
}

fun box(): String = if (foo(false) == 42 && foo(true) == 42) "OK" else "fail"
