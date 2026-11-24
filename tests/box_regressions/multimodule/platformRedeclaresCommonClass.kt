// LANGUAGE: +MultiPlatformProjects
// TARGET_BACKEND: JVM, JVM_IR
// kotlinc-native crashes actualizing a platform class over a class of a library the common
// fragment depends on (KT-89466), so Native is pinned by tests/kotlinc_defects_e2e.rs instead.

// MODULE: lib-common
class Some(val origin: String = "common")

object Single {
    fun origin(): String = "common"
}

// MODULE: lib-platform()()(lib-common)

// MODULE: app-common(lib-common)
fun accept(s: Some) {}

// MODULE: app-platform(lib-platform)()(app-common)
class Some(val origin: String = "platform")

object Single {
    fun origin(): String = "platform"
}

fun box(): String {
    val some: Any = Some()
    if (some !is Some) return "type test"
    if (Single.origin() != "platform") return "object"
    return if (Some().origin == "platform") "OK" else "constructor"
}
