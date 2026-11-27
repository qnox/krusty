OPTIONAL_JVM_INLINE_ANNOTATION
value class Id(val raw: Int)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Level(val id: Id = Id(9))

fun box(): String {
    if (Level() != Level(Id(9))) return "default value class"
    if (Level().id.raw != 9) return "default raw"
    if (Level(Id(2)) == Level()) return "supplied value class"
    return "OK"
}
