// A class is an instance of interfaces another file declares.
// FILE: lib.kt
interface A
interface B

// FILE: main.kt
class C : A, B

fun check(value: Any): Boolean = value is A && value is B

fun box(): String = if (check(C())) "OK" else "FAIL"
