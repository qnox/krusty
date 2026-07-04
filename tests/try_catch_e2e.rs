//! `try { … } catch (e: E) { … }` as both expression and statement, including a throwing body caught
//! by the handler. Round-tripped against the JVM under `-Xverify:all`.

mod common;

#[test]
fn try_catch_runs() {
    let src =
        "fun mightThrow(b: Boolean): Int { if (b) throw RuntimeException(\"x\"); return 1 }\n\
fun box(): String {\n\
val r = try { mightThrow(true) } catch (e: RuntimeException) { 42 }\n\
if (r != 42) return \"f1\"\n\
val s = try { mightThrow(false) } catch (e: RuntimeException) { 0 }\n\
if (s != 1) return \"f2\"\n\
val t = \"O\" + try { throw Exception(\"boom\") } catch (e: Exception) { \"K\" }\n\
if (t != \"OK\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "T");
}
