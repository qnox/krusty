@Target(AnnotationTarget.TYPE)
annotation class Tag

fun call(block: @Tag (() -> String)? = null): String = block?.invoke() ?: "O"

fun <T> pick(value: T): T = value

fun box(): String {
    val none = pick<@Tag (() -> String)?>(null)
    return call(none) + call { "K" }
}
