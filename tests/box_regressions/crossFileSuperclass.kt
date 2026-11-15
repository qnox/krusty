// FILE: base.kt
package p

interface Named {
    fun name(): String
    fun greet(): String = "hi " + name()
}

abstract class Resource(val label: String) : Named {
    var opened = 0
    abstract fun open(): String
    open fun suffix(): String = "!"
    override fun name(): String = label
}

// FILE: file.kt
package p

class FileResource(label: String, private val content: String) : Resource(label) {
    override fun open(): String {
        opened++
        return content
    }

    override fun suffix(): String = super.suffix() + "?"

    fun read(): String = open() + suffix()
}

fun box(): String {
    val resource = FileResource("file", "K")
    if (resource.read() != "K!?") return "fail read"
    if (resource.opened != 1) return "fail opened ${resource.opened}"
    val named: Named = resource
    if (named.greet() != "hi file") return "fail greet ${named.greet()}"
    if (resource.label != "file") return "fail label"
    return "OK"
}
