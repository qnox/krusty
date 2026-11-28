// TARGET_BACKEND: JVM
// PLATFORM-SPECIFIC: the inherited members are Java declarations, whose type variables are
// flexible where a signature writes them as a classifier argument. `Class<? extends T>` with
// `T = File` is `Class<out File!>`, so an override may state `Class<out File?>` or
// `Class<out File>`. Through a raw Java supertype the slot is erased: `RawImpl`'s own
// `read(String, Class)` overrides it, and a Kotlin override states `Class<*>?`. The Java
// parameters are spelled `p0`, `p1` because a bridge names its locals after the overridden
// declaration, which kotlinc reads from the Java source and krusty from its compiled class.
// FILE: Ser.java
public abstract class Ser<T> {
    public Ser(boolean b) {}
    public abstract void write(String p0, T p1);
    public abstract T read(String p0, Class<? extends T> p1);
}

// FILE: RawImpl.java
@SuppressWarnings({"rawtypes", "unchecked"})
public class RawImpl extends Ser {
    public RawImpl() { super(true); }
    public void write(String p0, Object p1) {}
    public Object read(String p0, Class p1) { return p0; }
}

// FILE: box.kt
import java.io.File

object NullableFileSer : Ser<File>(true) {
    override fun write(o: String, obj: File?) {}
    override fun read(i: String, type: Class<out File?>): File? = File(i)
}

object FileSer : Ser<File>(true) {
    override fun write(o: String, obj: File) {}
    override fun read(i: String, type: Class<out File>): File = File(i)
}

class KotlinRaw : RawImpl() {
    override fun read(i: String?, type: Class<*>?): Any? = "kotlin " + i
}

fun box(): String {
    if (NullableFileSer.read("O", File::class.java)?.path != "O") return "nullable face"
    if (FileSer.read("K", File::class.java).path != "K") return "not-null face"
    val nullable: Ser<File> = NullableFileSer
    if (nullable.read("virtual", File::class.java).path != "virtual") return "virtual nullable face"
    if (RawImpl().read("raw", null) != "raw") return "raw supertype"
    val raw: RawImpl = KotlinRaw()
    if (raw.read("raw", String::class.java) != "kotlin raw") return "override through a raw supertype"
    return "OK"
}
