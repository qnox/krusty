// MODULE: common
// FILE: common.kt
package p

expect interface Closer {
    fun close()
}

abstract class Resource : Closer

// MODULE: main()()(common)
// FILE: main.kt
package p

interface PlatformCloser {
    fun close()
    fun twice() {
        close()
        close()
    }
}

actual typealias Closer = PlatformCloser

class FileResource : Resource() {
    var closed = 0
    override fun close() {
        closed++
    }
}

fun box(): String {
    val resource = FileResource()
    resource.twice()
    return if (resource.closed == 2) "OK" else "closed ${resource.closed} times"
}
