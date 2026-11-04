package app

@Target(
    AnnotationTarget.TYPE, AnnotationTarget.CLASS, AnnotationTarget.FUNCTION,
    AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.PROPERTY, AnnotationTarget.CONSTRUCTOR
)
annotation class Paired(val first: Int, val second: Int)
@Target(AnnotationTarget.TYPE)
annotation class Entries(vararg val entries: Paired)
class Item

@Paired(second = 8, first = 7)
class Decorated

class Constructed @Paired(second = 10, first = 9) constructor()

@Paired(second = 12, first = 11)
fun selected(@Paired(second = 14, first = 13) item: Item): Item = item

@Paired(second = 16, first = 15)
val marked: Int = 1

fun grouped(item: @Entries(
    Paired(second = 2, first = 1),
    Paired(second = 4, first = 3)
) Item): Item = item

fun explicit(item: @Entries(entries = [
    Paired(second = 6, first = 5)
]) Item): Item = item

fun empty(item: @Entries Item): Item = item
