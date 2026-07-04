//! Two backend paths the corpus underexercises: a `data class` whose fields are `Double`/`Long`/
//! `Float` (its synthesized `hashCode` calls the static `Double.hashCode`/`Long.hashCode`/
//! `Float.hashCode` helpers) and a lambda converted to a `void`-returning SAM interface (`Runnable`),
//! whose bridge body loads `Unit.INSTANCE` after the call.

mod common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn data_class_float_field_hashcode() {
    run_ok(
        "DataHash",
        "data class Nums(val d: Double, val l: Long, val f: Float)\n\
         fun box(): String {\n\
         val a = Nums(1.5, 7L, 2.5f)\n\
         val b = Nums(1.5, 7L, 2.5f)\n\
         if (a != b) return \"eq\"\n\
         if (a.hashCode() != b.hashCode()) return \"hc\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn lambda_to_void_sam() {
    run_ok(
        "SamVoid",
        "fun box(): String {\n\
         var x = 0\n\
         val r = Runnable { x = 5 }\n\
         r.run()\n\
         return if (x == 5) \"OK\" else \"x=$x\"\n\
         }\n",
    );
}
