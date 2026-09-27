// Kotlin's answers for `floating_range.c`: `Double` and `Float` ranges' members.
fun box(): String = buildString {
    val d = 1.0..2.0
    appendLine("$d ${d.isEmpty()} ${1.0 in d} ${2.0 in d} ${1.5 in d} ${0.5 in d} " +
        "${Double.NaN in d} ${d.hashCode()} ${d.start} ${d.endInclusive}")
    val e = 2.0..1.0
    val n = Double.NaN..1.0
    val nn = Double.NaN..Double.NaN
    appendLine("$e ${e.isEmpty()} ${e.hashCode()} $n ${n.isEmpty()} ${1.0 in n} $nn " +
        "${nn == nn} ${nn.hashCode()} ${e == n}")
    appendLine("${d == (1.0..2.0)} ${(0.0..1.0) == (-0.0..1.0)} ${(0.0..1.0).hashCode()} " +
        "${(-0.0..1.0).hashCode()} ${d == (1.0..3.0)}")
    val f = 1.5f..2.5f
    appendLine("$f ${f.isEmpty()} ${2.0f in f} ${2.6f in f} ${f.hashCode()} " +
        "${(f as Any) == (1.5..2.5)} ${f == (1.5f..2.5f)}")
    val g = 0.1f..0.2f
    appendLine("$g ${g.hashCode()} ${(-0.0f..1.0f) == (0.0f..1.0f)} ${(1e10..1e-5).isEmpty()} " +
        "${(1e-5..1e10)}")
    appendLine("${(Double.NEGATIVE_INFINITY..Double.POSITIVE_INFINITY)} " +
        "${Double.MAX_VALUE in (Double.NEGATIVE_INFINITY..Double.POSITIVE_INFINITY)}")
}
