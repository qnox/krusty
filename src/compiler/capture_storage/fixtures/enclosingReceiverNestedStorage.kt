class Outer(val value: Int) {
    fun check(): Int {
        class Local {
            inner class Nested {
                fun read(): Int = value
            }
            fun read(): Int = Nested().read()
        }
        return Local().read()
    }
}
fun box(): String = if (Outer(42).check() == 42) "OK" else "FAIL"
