fun interface Source { fun get(): Int }
fun one(): Int = 1
fun read(source: Source): Int = source.get()
fun box(): String = if (read (::one) == 1) "OK" else "fail"
