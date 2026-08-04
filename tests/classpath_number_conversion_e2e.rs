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
