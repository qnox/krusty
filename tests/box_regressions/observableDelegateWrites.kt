// Delegates.observable runs its callback after each write, with the delegation's own KProperty:
// a callback reading the property sees the value just stored.
import kotlin.properties.Delegates
var log = ""
var seen = ""
var watched: String by Delegates.observable("start") { property, old, new ->
    log += property.name + ":" + old + "->" + new + ";"
    seen += watched
}
fun box(): String {
    if (watched != "start") return "fail initial " + watched
    if (log != "") return "fail early " + log
    watched = "middle"
    watched = "end"
    if (watched != "end") return "fail final " + watched
    if (log != "watched:start->middle;watched:middle->end;") return "fail log " + log
    // The callback runs after the write, so it reads the value that was just stored.
    if (seen != "middleend") return "fail order " + seen
    return "OK"
}
