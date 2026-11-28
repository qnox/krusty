// LANGUAGE: +MultiPlatformProjects
// The whole platformRedeclaresCommonClass.kt graph, plus code of lib-common that builds, reads and
// tests instances of the classes app-platform redeclares. kotlinc links lib-common to the
// redeclarations too. krusty lowers lib-common before app-platform, against lib-common's own
// classes, so a KLIB target declines the redeclaration instead of answering differently.

// MODULE: lib-common
class Some(val origin: String = "common")

object Single {
    fun origin(): String = "common"
}

fun libTypeTest(value: Any): Boolean = value is Some

fun libObjectRead(): String = Single.origin()

fun libConstructedOrigin(): String = Some().origin

// MODULE: lib-platform()()(lib-common)

// MODULE: app-common(lib-common)
fun accept(s: Some) {}

fun commonTypeTest(value: Any): Boolean = value is Some

fun commonObjectRead(): String = Single.origin()

fun commonConstruct(): Any = Some()

fun commonConstructedOrigin(): String = Some().origin

// MODULE: app-platform(lib-platform)()(app-common)
class Some(val origin: String = "platform")

object Single {
    fun origin(): String = "platform"
}

fun box(): String {
    val some: Any = Some()
    if (some !is Some) return "type test"
    if (Single.origin() != "platform") return "object"
    if (Some().origin != "platform") return "constructor"
    accept(Some())
    if (!commonTypeTest(some)) return "common type test"
    if (commonObjectRead() != "platform") return "common object read: ${commonObjectRead()}"
    if (commonConstruct() !is Some) return "common construction is not the platform class"
    if (commonConstructedOrigin() != "platform") return "common construct: ${commonConstructedOrigin()}"
    if (!libTypeTest(some)) return "library type test"
    if (libObjectRead() != "platform") return "library object read: ${libObjectRead()}"
    if (libConstructedOrigin() != "platform") return "library construct: ${libConstructedOrigin()}"
    return "OK"
}
