interface Dialect { val name: String }
class Pg : Dialect {
    override val name = "pg"
    fun pgOnly() = "P"
}
class Plain : Dialect {
    override val name = "plain"
}

class Items(val first: String, val second: String) {
    val size: Int get() = 2
    fun decorate(suffix: String): String = first + suffix + second + suffix
    fun firstOrNull(): String? = if (first.length == 0) null else first
}

class Box(val items: Items)

fun Dialect.viaRun(value: Items): String =
    if (this is Pg) value.run { decorate("!") + size } else "none"

fun Dialect.viaSafeRun(value: Items?): String =
    if (this is Pg) value?.run { decorate("?") } ?: "empty" else "none"

fun Dialect.viaWith(box: Box): String = when (this) {
    is Pg -> with(box) { items.decorate("#") }
    else -> "none"
}

fun Dialect.outerLabeled(value: Items): String =
    if (this is Pg) value.run { this@outerLabeled.pgOnly() + first } else "none"

fun Dialect.afterGuard(value: Items): String = when {
    this is Pg && value.size > 0 -> value.apply { size }.first
    else -> "none"
}

fun box(): String {
    val pg = Pg()
    val plain = Plain()
    if (pg.viaRun(Items("a", "b")) != "a!b!2") return "run"
    if (plain.viaRun(Items("a", "b")) != "none") return "run plain"
    if (pg.viaSafeRun(Items("a", "b")) != "a?b?" || pg.viaSafeRun(null) != "empty") return "safe run"
    if (pg.viaWith(Box(Items("x", "y"))) != "x#y#" || plain.viaWith(Box(Items("x", "y"))) != "none") return "with"
    if (pg.outerLabeled(Items("a", "b")) != "Pa") return "labeled"
    if (pg.afterGuard(Items("q", "r")) != "q") return "guard"
    return "OK"
}
