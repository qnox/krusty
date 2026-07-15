//! A value-class-derived local captured into a LAMBDA that string-interpolates it alongside another
//! operand. The lambda's body is a separate method (`make$lambda$0`) with its own value-index numbering;
//! the per-function value-class box/unbox analysis must NOT reach into it from the enclosing `make`
//! (whose slot for the same index holds the value class `Id`), or it mis-boxes the OTHER interpolation
//! operand with `Id.box-impl` → "Bad type on operand stack" at load. Mission-core hit:
//! `MissionChangeService.approveChange`'s `logger.info { "…${id.value}… $scheduledAt" }`.
//! Needs the JVM toolchain + kotlin-stdlib; skips otherwise.
use super::common;

#[test]
fn value_class_derived_local_in_lambda_template() {
    const SRC: &str = "@JvmInline value class Id(val value: String)\n\
        fun sink(f: () -> String): String = f()\n\
        fun make(id: Id, n: Int): String { val x = id.value; return sink { \"id=$x n=$n\" } }\n\
        fun box(): String {\n\
            val r = make(Id(\"abc\"), 7)\n\
            return if (r == \"id=abc n=7\") \"OK\" else \"FAIL:\" + r\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}
