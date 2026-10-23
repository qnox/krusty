package annotations

import annotations.Plain as Bare
import annotations.Many as Several

@Bare
class Aliased @Bare constructor(@Bare val x: Int, @Bare plain: Int) {
    @Bare
    constructor(@Bare s: String) : this(0, 0)

    @Bare
    val y: Int = 1

    @Several("m")
    fun member(@Bare p: Int) {}

    @get:Bare
    @setparam:Bare
    var z: Int = 2
}

@Several("v")
fun aliasedFunction(@Bare q: Int) {}

@Nest(Both(1, 2), Several("n"))
fun aliasedNested() {}
