package fixture

class Cell<T>(val value: T)
class Readings<T>(val first: T, var second: T?)
val topValue: Int get() = 7

fun makeStringCell(value: String): Cell<String> = Cell(value)
fun makeIntCell(value: Int): Cell<Int> = Cell(value)
fun makeLongCell(value: Long): Cell<Long> = Cell(value)
fun makeNullableCell(value: String?): Cell<String?> = Cell(value)
fun makeReadings(first: String, second: String?): Readings<String> = Readings(first, second)
fun readStringCell(cell: Cell<String>): String = cell.value
fun readIntCell(cell: Cell<Int>): Int = cell.value
inline fun inlineReadStringCell(cell: Cell<String>): String = cell.value
inline fun inlineReadIntCell(cell: Cell<Int>): Int = cell.value

fun constructThenLocalClass(value: String): Any {
    val cell = Cell(value)
    class Local
    return cell
}
