object Events {
    var text = ""
}

object Sink {
    var value = ""
    init { Events.text += "I" }
}

fun next(): String {
    Events.text += "R"
    return "OK"
}

fun box(): String {
    Sink.value += next()
    return if (Sink.value == "OK" && Events.text == "IR") "OK" else Events.text + ":" + Sink.value
}
