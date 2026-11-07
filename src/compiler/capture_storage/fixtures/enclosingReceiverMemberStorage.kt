class Outer(val value: Int) {
    fun check(seed: Int): Int {
        class Local {
            fun read(): Int = value + seed
        }
        val first = Local()
        val second = Local()
        return first.read() + second.read()
    }
}
fun box(): String = if (Outer(20).check(1) == 42) "OK" else "FAIL"
