// An omitted parameter whose default is itself a same-module inline call evaluates the nested
// expansion inside the callee's frame, with every parameter slot live while it runs.
var trace = ""
inline fun mark(value: Int): Int {
    trace += value
    return value
}
inline fun withNestedDefault(a: Int = mark(1)): Int = a + 10
inline fun twoNestedDefaults(a: Int = mark(2), b: Int = mark(3) + a): Int = a * 100 + b
inline fun readsSuppliedOperand(a: Int, b: Int = mark(4) + a): Int = a + b

fun box(): String {
    if (withNestedDefault() != 11 || trace != "1") return "FAIL: single $trace"
    trace = ""
    if (twoNestedDefaults() != 205 || trace != "23") return "FAIL: two ${twoNestedDefaults()} $trace"
    trace = ""
    if (twoNestedDefaults(7) != 710 || trace != "3") return "FAIL: supplied first $trace"
    trace = ""
    if (readsSuppliedOperand(5) != 14 || trace != "4") return "FAIL: supplied operand $trace"
    return "OK"
}
