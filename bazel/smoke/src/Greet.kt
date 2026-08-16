package smoke

// Deliberately touches the platform classpath (`java.util.ArrayList`): with a wrong or missing
// JAVA_HOME krusty resolves no `java.*` type, so this file fails to compile. A fixture whose
// sources only use `String` builds green with JAVA_HOME unset and proves nothing about it.
class Greeter(private val who: String) {
    fun greet(): String {
        val parts = java.util.ArrayList<String>()
        parts.add("hello")
        parts.add(who)
        return parts.joinToString(" ")
    }
}
