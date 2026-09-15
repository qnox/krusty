use super::common;

#[test]
fn mapped_number_conversions_use_jdk_members_without_stdlib() {
    let jdk = common::jdk_modules();
    let source = r#"
        import java.util.concurrent.atomic.AtomicInteger

        fun box(): String {
            val concrete = AtomicInteger(7)
            if (concrete.toInt() != 7) return "concrete"

            val number: Number = concrete
            if (number.toByte() != 7.toByte()) return "byte"
            if (number.toShort() != 7.toShort()) return "short"
            if (number.toInt() != 7) return "int"
            if (number.toLong() != 7L) return "long"
            if (number.toFloat() != 7.0f) return "float"
            if (number.toDouble() != 7.0) return "double"
            return "OK"
        }
    "#;

    let Some(output) = common::compile_and_run_box(source, "Main", &[], Some(jdk.as_path())) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "OK");
}

/// A classpath method's RESULT is typed by what it declares, not by the slot the JVM carries it in.
///
/// `java.lang.Number.byteValue()B` is Kotlin's `Number.toByte(): Byte`, and the descriptor `B` was
/// being read the way the JVM *stack* reads it — as `Int`. Every check above still passed, because
/// `7.toByte() == 7` either way and a byte and an int share a stack slot on this backend; the
/// difference only shows where the result is asked as a TYPE. kotlinc 2.4.10, asked directly,
/// answers `Byte`/`Short` for these.
#[test]
fn a_number_conversion_answers_the_type_it_declares() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let source = r#"
        fun name(value: Any): String = value::class.simpleName ?: "null"
        fun box(): String {
            val wide: Number = 258L
            val parts = listOf(
                name(wide.toByte()), name(wide.toShort()), name(wide.toInt()),
                name(wide.toLong()), name(wide.toFloat()), name(wide.toDouble()),
            )
            return parts.joinToString("/")
        }
    "#;

    let Some(output) = common::compile_and_run_box(source, "Main", &[stdlib], Some(jdk.as_path()))
    else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "Byte/Short/Int/Long/Float/Double");
}
