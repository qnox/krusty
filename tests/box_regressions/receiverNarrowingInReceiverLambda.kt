interface Dialect { val name: String }
class Pg : Dialect {
    override val name = "pg"
    fun pgOnly() = "P"
}
class Plain : Dialect {
    override val name = "plain"
}

class Box(val items: List<String>)

fun Dialect.viaRun(value: List<String>): String =
    if (this is Pg) value.run { map { it + "!" }.joinToString() + size } else "none"

fun Dialect.viaSafeRun(value: List<String>?): String =
    if (this is Pg) value?.takeIf { it.isNotEmpty() }?.run { map { it + "?" }.joinToString() } ?: "empty" else "none"

fun Dialect.viaWith(box: Box): String = when (this) {
    is Pg -> with(box) { items.map { it.uppercase() }.joinToString() }
    else -> "none"
}

fun Dialect.outerLabeled(value: List<String>): String =
    if (this is Pg) value.run { this@outerLabeled.pgOnly() + first() } else "none"

fun Dialect.afterGuard(value: List<String>): String = when {
    this is Pg && value.isNotEmpty() -> value.apply { size }.joinToString()
    else -> "none"
}

fun box(): String {
    val pg = Pg()
    val plain = Plain()
    if (pg.viaRun(listOf("a", "b")) != "a!, b!2") return "run"
    if (plain.viaRun(listOf("a")) != "none") return "run plain"
    if (pg.viaSafeRun(listOf("a")) != "a?" || pg.viaSafeRun(emptyList()) != "empty") return "safe run"
    if (pg.viaWith(Box(listOf("x", "y"))) != "X, Y" || plain.viaWith(Box(listOf("x"))) != "none") return "with"
    if (pg.outerLabeled(listOf("a", "b")) != "Pa") return "labeled"
    if (pg.afterGuard(listOf("q")) != "q") return "guard"
    return "OK"
}
