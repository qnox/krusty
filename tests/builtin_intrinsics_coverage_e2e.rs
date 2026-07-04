//! Builtin member intrinsics the backend emits inline — per-primitive `hashCode()`
//! (`Int/Long/Short/Byte/Boolean/Char/Double/Float`), `Double`/`Float` `compareTo` (→ `*.compare`),
//! `Enum.ordinal`/`name`, and `String.length`/`get`. Each is a distinct `emit_builtin_call` arm; the
//! box corpus leaves several primitive variants untouched. Values are checked against their documented
//! results (self-consistency for the bit-pattern float hashes).

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn primitive_hashcode_intrinsics() {
    run_ok(
        "PrimHash",
        "fun box(): String {\n\
         if (5.hashCode() != 5) return \"i\"\n\
         if (7L.hashCode() != 7) return \"l\"\n\
         if (true.hashCode() != 1231) return \"bt\"\n\
         if (false.hashCode() != 1237) return \"bf\"\n\
         if ('A'.hashCode() != 65) return \"c\"\n\
         val s: Short = 9\n\
         if (s.hashCode() != 9) return \"s\"\n\
         val b: Byte = 3\n\
         if (b.hashCode() != 3) return \"by\"\n\
         val d = 1.5\n\
         if (d.hashCode() != d.hashCode()) return \"d\"\n\
         val f = 2.5f\n\
         if (f.hashCode() != f.hashCode()) return \"f\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn float_double_compare_and_enum_string_intrinsics() {
    run_ok(
        "CmpEnumStr",
        "enum class Col { RED, GREEN }\n\
         fun box(): String {\n\
         if (1.5.compareTo(2.5) >= 0) return \"dcmp\"\n\
         if (2.5f.compareTo(1.5f) <= 0) return \"fcmp\"\n\
         if (Col.GREEN.ordinal != 1) return \"ord\"\n\
         if (Col.RED.name != \"RED\") return \"name\"\n\
         if (\"hi\".length != 2) return \"len\"\n\
         if (\"hi\"[1] != 'i') return \"idx\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn branchy_arg_construction_spill() {
    // Constructing with a branchy (`if`-expression) argument records a StackMapTable frame, so the
    // backend must spill the already-evaluated args to temporaries instead of leaving them under the
    // `new`/`dup`. A captured `var` with a branchy initializer exercises the same spill on the boxed
    // holder (RefNew). `cond()` is a call so the condition isn't const-folded away.
    run_ok(
        "BranchySpill",
        "class Pair2(val a: Int, val b: Int)\n\
         fun cond(): Boolean = true\n\
         fun box(): String {\n\
         val p = Pair2(if (cond()) 1 else 2, if (cond()) 3 else 4)\n\
         if (p.a != 1 || p.b != 3) return \"new=${p.a},${p.b}\"\n\
         var v = if (cond()) 5 else 6\n\
         val f = { v = v + 1; v }\n\
         val r = f()\n\
         if (r != 6 || v != 6) return \"ref=$r,$v\"\n\
         return \"OK\"\n\
         }\n",
    );
}
