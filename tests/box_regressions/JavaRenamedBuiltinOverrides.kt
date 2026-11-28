// TARGET_BACKEND: JVM
// FULL_JDK
// PLATFORM-SPECIFIC: `charAt` is the JVM name of `CharSequence.get`. A Java class that declares
// both `charAt(int)` and `get(int)`, as `java.nio.CharBuffer` does, has a `get` of its own that
// does not override the builtin, so a call keeps its name; `charAt` alone is the builtin's override.
// A Kotlin fake override of a Java method takes the Java declaration's own parameter names when
// the compiler reads them from Java source, here `index` for the `get(int)` that delegates to
// `charAt(int index)`. `Both` refers to a Kotlin object, so the Java is compiled beside the Kotlin
// sources rather than first. In `InheritsBoth` that fake override `get` replaces the Java `get`
// and answers a call made through `Both` too.
// FILE: Both.java
public class Both implements CharSequence {
    public int length() { return 2; }
    public char charAt(int index) { return 'c'; }
    public char get(int index) { return 'g'; }
    public CharSequence subSequence(int start, int end) { return ""; }
    public String toString() { return Marker.NAME; }
}

// FILE: JvmNameOnly.java
public class JvmNameOnly implements CharSequence {
    public int length() { return 1; }
    public char charAt(int index) { return 'j'; }
    public CharSequence subSequence(int start, int end) { return ""; }
    public String toString() { return "JvmNameOnly"; }
}

// FILE: box.kt
object Marker {
    const val NAME = "Both"
}

class InheritsBoth : Both()

class OverridesJvmName : JvmNameOnly() {
    override fun get(index: Int): Char = 'k'
}

fun box(): String {
    val both = Both()
    val bothSequence: CharSequence = both
    val jvmNameOnly = JvmNameOnly()
    val overriding = OverridesJvmName()
    val overridingSequence: CharSequence = overriding
    val buffer = java.nio.CharBuffer.wrap("xy")
    val bufferSequence: CharSequence = buffer
    val inherits = InheritsBoth()
    val inheritsAsBoth: Both = inherits
    val inheritsSequence: CharSequence = inherits
    val read = buildString {
        append(both.get(0))
        append(both[0])
        append(bothSequence.get(0))
        append(bothSequence[0])
        append(jvmNameOnly.get(0))
        append(jvmNameOnly[0])
        append(overriding[0])
        append(overridingSequence[0])
        append(overriding.length)
        append(buffer.get(1))
        append(buffer[1])
        append(bufferSequence[1])
        append(inherits.get(0))
        append(inherits[0])
        append(inheritsAsBoth.get(0))
        append(inheritsSequence[0])
        append(inherits.length)
        append(inherits.toString())
    }
    return if (read == "ggccjjkk1yyycccc2Both") "OK" else read
}
