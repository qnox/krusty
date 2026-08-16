package examples.formatting

class Repository<T:Comparable<T>>(val items:MutableList<T>,val capacity:Int)

fun <T> firstOrNull(items:List<T>):T?=items.firstOrNull()

fun format(label:String,value:Int)=when{
value<0->"$label: negative"
value==0->"$label: zero"
else->"$label: $value"
}
