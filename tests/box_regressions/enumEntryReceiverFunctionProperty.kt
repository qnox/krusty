var functionGetterCalls = 0

enum class EntryFunctionValue {
    X {
        val suffix: String.() -> String = { this + "K" }
        val plain: () -> String
            get() {
                functionGetterCalls += 1
                return { "O" }
            }
        val optional: (() -> String)? = { "K" }
        val initialized: String = "O".suffix()
        val initializedPlain: String = plain()
        val reader = object {
            fun result(): String {
                val extended = "O".suffix()
                val ordinary = plain()
                val safe = optional?.invoke()
                return if (extended == "OK" && ordinary == "O" && safe == "K") "OK"
                    else extended + ordinary + safe
            }
        }
        override fun result(): String {
            val answer = reader.result()
            return if (initialized == "OK" && initializedPlain == "O" && functionGetterCalls == 2)
                answer else initialized + initializedPlain + functionGetterCalls
        }
    };
    abstract fun result(): String
}

fun box(): String = EntryFunctionValue.X.result()
