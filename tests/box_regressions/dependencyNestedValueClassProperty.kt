// MODULE: lib
// FILE: Levels.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class Id(val raw: Int)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Level(val id: Id) {
    fun getId(other: Id): Int = other.raw + id.raw
}

// MODULE: box(lib)
// FILE: box.kt
fun box(): String {
    val level = Level(Id(2))
    if (level.id != Id(2)) return "nested property"
    if (level.id.raw != 2) return "nested raw"
    if (level.getId(Id(3)) != 5) return "overload named like the getter"
    return "OK"
}
