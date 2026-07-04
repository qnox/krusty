//! A SYNTACTIC trailing lambda (`f(a) { … }`) always binds to the callee's LAST parameter; a middle
//! parameter with a default that receives no positional argument takes its default. This is the shape
//! `NavHost(navController, startDestination) { … }` (the defaulted `modifier` is skipped, the trailing
//! lambda fills `builder`). Both the checker (arity/assignability) and lowering (`$default` slot
//! placement) must route the lambda to the last slot, not the next free positional one.

mod common;

#[test]
fn trailing_lambda_skips_defaulted_middle_param() {
    // `host` has a defaulted MIDDLE parameter `modifier`; the call omits it and passes a trailing lambda
    // for the final `builder` parameter. `builder` runs and appends to a StringBuilder we observe.
    const SRC: &str = "\
fun host(prefix: String, modifier: String = \"M\", builder: (StringBuilder) -> Unit): String {\n\
  val sb = StringBuilder()\n\
  sb.append(prefix)\n\
  sb.append(modifier)\n\
  builder(sb)\n\
  return sb.toString()\n\
}\n\
fun box(): String {\n\
  val a = host(\"p\") { it.append(\"B\") }\n\
  if (a != \"pMB\") return \"f1: \" + a\n\
  val b = host(\"p\", \"X\") { it.append(\"B\") }\n\
  if (b != \"pXB\") return \"f2: \" + b\n\
  return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "D");
}
