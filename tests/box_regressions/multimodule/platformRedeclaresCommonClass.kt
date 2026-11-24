// LANGUAGE: +MultiPlatformProjects
// TARGET_BACKEND: JVM, JVM_IR
// PLATFORM-SPECIFIC: kotlinc-native crashes actualizing a platform class over a class of a
// library the common fragment depends on (KT-89466), so it has no answer to compare krusty's
// Native result with. That is a kotlinc defect, not a language rule: Native's outcome is the
// subject of tests/kotlinc_defects_e2e.rs, which pins the crash. Wasm shares this program once
// its link table accepts a redeclaration.

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
