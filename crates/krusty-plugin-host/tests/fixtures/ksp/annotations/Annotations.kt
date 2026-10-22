package annotations

import kotlin.reflect.KClass

enum class Level { LOW, HIGH }

annotation class Inner(val name: String = "inner")

annotation class Everything(
    val text: String,
    val number: Int = 7,
    val big: Long = 1L shl 40,
    val small: Byte = 3,
    val short: Short = 4,
    val ratio: Float = 0.5f,
    val exact: Double = 2.5,
    val letter: Char = 'k',
    val flag: Boolean = true,
    val level: Level = Level.LOW,
    val kind: KClass<*> = String::class,
    val inner: Inner = Inner(),
    val names: Array<String> = ["a", "b"],
    val numbers: IntArray = [1, 2],
    val levels: Array<Level> = [],
    val kinds: Array<KClass<*>> = [Int::class],
)

annotation class Plain

annotation class Many(vararg val values: String)

annotation class Both(val a: Int, val b: Int)

annotation class Nest(val both: Both, val many: Many)

const val PREFIX = "pre"

@Everything("class", number = 1 + 2, level = Level.HIGH, kind = List::class, inner = Inner("x"), names = ["$PREFIX-fix"])
@Plain
class Subject @Plain constructor(@Plain val id: Int, @param:Many("p") @property:Plain val other: String) {
    @Many("one", "two")
    fun act(@Everything(text = "param", numbers = [3]) input: String, @Many vararg rest: Int): String = input

    @field:Plain
    @get:Many("g")
    @set:Many("s")
    @setparam:Plain
    var state: Int = 0

    @Everything(text = "flags", small = 9, short = -1, ratio = 1.25f, exact = -0.0, letter = '\'', flag = false, kinds = [String::class, Map::class])
    val other2: Long get() = 0L
}

@Many(*["spread", "it"])
fun topLevel() {}

@Everything(level = Level.HIGH, text = "swapped", number = 5)
@Many(*[])
fun reordered() {}

@Suppress("UNUSED")
@JvmName("named")
fun library(@Plain value: Int) {}

@Plain
class Holder @Plain constructor(@Plain val p: Int, @Plain plain: Int) {
    @Plain
    val q: Int = 1

    @get:Plain
    @setparam:Plain
    var r: Int = 2

    constructor(@Plain s: String) : this(0, 0)
}

@Nest(many = Many(), both = Both(b = 2, a = 1))
fun nestedNamed() {}

@Nest(Both(1, 2), Many(*[]))
fun nestedEmptySpread() {}

@Nest(Both(a = 1, b = 2), Many(*["x", "y"]))
fun nestedSpread() {}

@Nest(Both(3, b = 4), Many("z"))
fun nestedPositional() {}
