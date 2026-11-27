// MODULE: lib
// FILE: Levels.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class Id(val raw: Int)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Level(val id: Id = Id(9))

OPTIONAL_JVM_INLINE_ANNOTATION
value class Tag(val label: String = "base")

// MODULE: box(lib)
// FILE: box.kt
fun box(): String {
    if (Level() != Level(Id(9))) return "default value class"
    if (Level(Id(2)) == Level()) return "supplied value class"
    if (Tag() != Tag("base")) return "default carrier"
    if (Tag("x") == Tag()) return "supplied carrier"
    return "OK"
}
