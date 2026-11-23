infix tailrec fun Int.infixWide(n: Int): Int = 1 + (this infixWide (n - 1))
infix tailrec fun Int.infixParenthesized(n: Int): Int = 1 + ((this + 0)   infixParenthesized   (n - 1))
infix tailrec fun Int.infixDotted(n: Int): Int = 1 + (this.infixDotted(n - 1))
class Operand(val x: Int) {
    tailrec operator fun plus(o: Operand): Operand = Operand(1) + (this + o) + o
    tailrec fun <T> typeArguments(n: T): Int = 1 + this.typeArguments<T>(n)
}
tailrec fun twoCalls(n: Int): Int = 1 + twoCalls(n)  +   twoCalls(n)
tailrec fun lineBreak(n: Int): Int = 2 + lineBreak (
    n)
