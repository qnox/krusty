// FILE: caller.kt
// Constructs classes another file declares, leaving constructor arguments to their defaults: a
// primary constructor whose default reads an earlier parameter, a secondary constructor, an inner
// class whose construction passes the enclosing instance first, and a generic class.
fun box(): String {
    if (Point().describe() != "1:2") return "Fail primary: " + Point().describe()
    if (Point(5).describe() != "5:6") return "Fail leading: " + Point(5).describe()
    if (Point(y = 9).describe() != "1:9") return "Fail named: " + Point(y = 9).describe()
    if (Point(3, 4).describe() != "3:4") return "Fail explicit: " + Point(3, 4).describe()
    if (Point("p").describe() != "p:7:8") return "Fail secondary: " + Point("p").describe()
    val outer = Outer("o")
    if (outer.Inner().text != "o-i") return "Fail inner: " + outer.Inner().text
    if (Holder<String>().value != null) return "Fail generic"
    if (Holder("v").value != "v") return "Fail generic explicit"
    return "OK"
}

// FILE: declarations.kt
class Point(val x: Int = 1, val y: Int = x + 1) {
    private var label = ""

    constructor(label: String, x: Int = 7) : this(x) {
        this.label = label + ":"
    }

    fun describe(): String = label + x + ":" + y
}

class Outer(val name: String) {
    inner class Inner(val suffix: String = "i") {
        val text: String get() = name + "-" + suffix
    }
}

class Holder<T>(val value: T? = null)
