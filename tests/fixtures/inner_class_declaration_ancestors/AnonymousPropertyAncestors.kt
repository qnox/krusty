fun propertyChain(): String {
    val root = object {
        private val middle = object {
            private val leaf = object {
                fun result(): String = "OK"
            }

            fun result(): String = leaf.result()
        }

        fun result(): String = middle.result()
    }
    return root.result()
}

fun box(): String = propertyChain()
