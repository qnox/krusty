// TARGET_BACKEND: JVM
// FULL_JDK
// PLATFORM-SPECIFIC: a Kotlin fake override of a Java method takes the Java declaration's own
// parameter names when the compiler reads them from Java source, here `index` for the `get(int)`
// that delegates to `charAt(int index)`. The Java refers to a Kotlin class, so it is compiled
// beside the Kotlin sources rather than first. `get` of its own does not override the builtin, so
// the fake override `get` replaces it and answers a call made through the Java `get` too.
// FILE: Both.java
public class Both implements CharSequence {
    public int length() { return 2; }
    public char charAt(int index) { return 'c'; }
    public char get(int index) { return 'g'; }
    public CharSequence subSequence(int start, int end) { return ""; }
    public String toString() { return Marker.NAME; }
}

// FILE: box.kt
object Marker {
    const val NAME = "Both"
}

class InheritsBoth : Both()

fun box(): String {
    val inherits = InheritsBoth()
    val inheritsAsBoth: Both = inherits
    val inheritsSequence: CharSequence = inherits
    val read = buildString {
        append(inherits.get(0))
        append(inherits[0])
        append(inheritsAsBoth.get(0))
        append(inheritsSequence[0])
        append(inherits.length)
        append(inherits.toString())
    }
    return if (read == "cccc2Both") "OK" else read
}
