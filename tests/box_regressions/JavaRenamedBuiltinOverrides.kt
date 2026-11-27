// TARGET_BACKEND: JVM
// FULL_JDK
// PLATFORM-SPECIFIC: `charAt` is the JVM name of `CharSequence.get`. A Java class that declares
// both `charAt(int)` and `get(int)`, as `java.nio.CharBuffer` does, has a `get` of its own that
// does not override the builtin, so a call keeps its name; `charAt` alone is the builtin's override.
// FILE: Both.java
public class Both implements CharSequence {
    public int length() { return 2; }
    public char charAt(int index) { return 'c'; }
    public char get(int index) { return 'g'; }
    public CharSequence subSequence(int start, int end) { return ""; }
    public String toString() { return "Both"; }
}

// FILE: JvmNameOnly.java
public class JvmNameOnly implements CharSequence {
    public int length() { return 1; }
    public char charAt(int index) { return 'j'; }
    public CharSequence subSequence(int start, int end) { return ""; }
    public String toString() { return "JvmNameOnly"; }
}

// FILE: box.kt
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
    }
    return if (read == "ggccjjkk1yyy") "OK" else read
}
