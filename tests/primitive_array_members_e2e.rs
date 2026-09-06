use super::common;

#[test]
fn explicit_primitive_array_members_use_array_operations_after_smart_cast() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "fun read(value: Any): Long {\n\
        if (value is LongArray) {\n\
            value.set(0, 42L)\n\
            return value.get(0)\n\
        }\n\
        return -1L\n\
    }\n\
    fun box(): String = if (read(LongArray(1)) == 42L) \"OK\" else \"FAIL\"\n";
    let classpath = [stdlib];
    let classes = common::compile_in_process(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("run box"),
        "OK"
    );
}
