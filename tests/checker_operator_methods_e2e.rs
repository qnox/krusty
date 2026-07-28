//! Type-checker arms for builtin operator *methods* called by name (not operator syntax) and vararg
//! argument assignability — `Int.unaryMinus()/unaryPlus()`, `Char.plus(n)/minus(n)/minus(c)`, and a
//! `vararg` call whose every element is checked against the element type. The corpus reaches the
//! operator syntax but not these explicit method forms.

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn char_operator_methods() {
    run_ok(
        "CharOps",
        "fun box(): String {\n\
         if ('a'.plus(1) != 'b') return \"cp\"\n\
         if ('b'.minus(1) != 'a') return \"cm\"\n\
         if ('c'.minus('a') != 2) return \"cc\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn zero_argument_primitive_operator_methods() {
    run_ok(
        "PrimitiveUnaryMethods",
        "fun <T> T.id() = this\n\
         fun box(): String {\n\
         if (true.not().id() != false) return \"not\"\n\
         if (true.compareTo(false).id() != 1) return \"bool compare\"\n\
         val bytePlus: Int = (1.toByte()).unaryPlus()\n\
         val shortMinus: Int = (1.toShort()).unaryMinus()\n\
         if (bytePlus.id() != 1) return \"byte+\"\n\
         if (shortMinus.id() != -1) return \"short-\"\n\
         if (2.unaryPlus().id() != 2) return \"int+\"\n\
         if (2L.unaryMinus().id() != -2L) return \"long-\"\n\
         if (2.0f.unaryMinus().id() != -2.0f) return \"float-\"\n\
         if (2.0.unaryPlus().id() != 2.0) return \"double+\"\n\
         if (java.lang.Float.floatToRawIntBits(0.0f.unaryMinus()) != -2147483648) return \"float zero\"\n\
         if (java.lang.Double.doubleToRawLongBits(0.0.unaryMinus()) != Long.MIN_VALUE) return \"double zero\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn vararg_argument_assignability() {
    run_ok(
        "VarargArgs",
        "fun sm(vararg xs: Int): Int {\n\
         var s = 0\n\
         for (x in xs) s += x\n\
         return s\n\
         }\n\
         fun box(): String {\n\
         if (sm(1, 2, 3, 4) != 10) return \"va=${sm(1, 2, 3, 4)}\"\n\
         if (sm() != 0) return \"empty\"\n\
         return \"OK\"\n\
         }\n",
    );
}
