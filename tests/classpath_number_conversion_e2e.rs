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

    let reference = common::kotlinc_box_result(source);
    assert_eq!(
        reference, "Byte/Short/Int/Long/Float/Double",
        "reference compiler disagrees"
    );

    let Some(output) = common::compile_and_run_box(source, "Main", &[stdlib], Some(jdk.as_path()))
    else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, reference);
}

#[test]
fn java_narrow_scalar_signatures_keep_their_declared_types() {
    let java = [(
        "Narrow.java".into(),
        r#"
        package fixtures;
        public final class Narrow {
            public static byte echoByte(byte value) { return value; }
            public static short echoShort(short value) { return value; }
            public static String select(byte value) { return "byte"; }
            public static String select(int value) { return "int"; }
        }
    "#
        .into(),
    )];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        panic!("javac must compile the narrow-scalar fixture");
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = vec![library, stdlib];
    let source = r#"
        import fixtures.Narrow

        fun name(value: Any): String = value::class.simpleName ?: "null"
        fun box(): String {
            val byte: Byte = 7
            val short: Short = 8
            return listOf(
                name(Narrow.echoByte(byte)),
                name(Narrow.echoShort(short)),
                name(Narrow.echoByte(7)),
                name(Narrow.echoShort(8)),
                Narrow.select(byte),
                Narrow.select(7),
            ).joinToString("/")
        }
    "#;

    let reference =
        common::kotlinc_box_result_with_classpath(source, std::slice::from_ref(&classpath[0]));
    assert_eq!(
        reference, "Byte/Short/Byte/Short/byte/int",
        "reference compiler disagrees"
    );

    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output, reference);
}
