// TARGET_BACKEND: JVM
// PLATFORM-SPECIFIC: the referenced inner class is a Java class.

// FILE: Outer.java
public class Outer {
    private final String prefix;

    public Outer(String prefix) {
        this.prefix = prefix;
    }

    public class Inner {
        public final String value;

        public Inner(String value) {
            this.value = prefix + value;
        }
    }
}

// FILE: main.kt
fun box(): String {
    val outer = Outer("O")
    val unbound: (Outer, String) -> Outer.Inner = Outer::Inner
    val bound: (String) -> Outer.Inner = outer::Inner
    return unbound(outer, "").value + bound("K").value.removePrefix("O")
}
