var setterCalls = 0

enum class EntrySetter {
    X {
        var saved = "K"
            set(value) {
                setterCalls += 1
                field = value
            }
        val writer = object {
            fun write(): String {
                saved = "O" + saved
                return saved
            }
        }
        override fun result() = writer.write()
    };
    abstract fun result(): String
}

fun box(): String {
    val answer = EntrySetter.X.result()
    return if (answer == "OK" && setterCalls == 1) "OK" else answer + setterCalls
}
