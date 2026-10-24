package dependency

import kotlin.reflect.KClass

annotation class Tag(val v: String, val level: Int = 1)

annotation class Choices(
    vararg val names: String,
    val types: Array<KClass<*>> = [],
    val tag: Tag = Tag("t"),
    val flag: Boolean,
) {
    annotation class Zeta

    annotation class Alpha
}
