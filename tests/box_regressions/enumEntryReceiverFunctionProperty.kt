var functionGetterCalls = 0
var receiverFunctionGetterCalls = 0
var receiverEvaluations = 0

fun selectedNullableReceiver(value: String?): String? {
    receiverEvaluations += 1
    return value
}

enum class EntryFunctionValue {
    X {
        val suffix: String.() -> String
            get() {
                receiverFunctionGetterCalls += 1
                return { this + "K" }
            }
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
                val safeReceiver = selectedNullableReceiver("O")?.suffix()
                val nullReceiver = selectedNullableReceiver(null)?.suffix()
                return if (extended == "OK" && ordinary == "O" && safe == "K" &&
                    safeReceiver == "OK" && nullReceiver == null) "OK"
                    else extended + ordinary + safe + safeReceiver + nullReceiver
            }
        }
        override fun result(): String {
            val answer = reader.result()
            return if (initialized == "OK" && initializedPlain == "O" && functionGetterCalls == 2 &&
                receiverFunctionGetterCalls == 3 && receiverEvaluations == 2)
                answer else initialized + initializedPlain + functionGetterCalls
        }
    };
    abstract fun result(): String
}

fun box(): String = EntryFunctionValue.X.result()
