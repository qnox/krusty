package examples.formatting

val template="""
line1
line2
""".trimIndent()

fun describe(x:Int)=when(x){1->"one" else->"other"}

fun render(name:String){
println("Hello, $name!")
}
