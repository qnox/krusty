package examples.formatting
import kotlin.math.max
import kotlin.math.abs
//spacing,semicolons and brace placement
class Box{
var label:String="box";
fun area(width:Int,height:Int):Int{
return width*height;
}
}
fun clamp(value:Int,limit:Int)=max(0,value)
