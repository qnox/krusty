use super::common;

#[test]
fn generic_static_field_binds_generic_call_result_at_runtime() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();

    let sources = [
        (
            "Key.java".into(),
            "package fixture;\n\
             public final class Key<T> {\n\
                 private final T value;\n\
                 public Key(T value) { this.value = value; }\n\
                 public T value() { return value; }\n\
             }\n"
            .into(),
        ),
        (
            "Payload.java".into(),
            "package fixture;\n\
             public final class Payload {\n\
                 public String message() { return \"OK\"; }\n\
             }\n"
            .into(),
        ),
        (
            "Fields.java".into(),
            "package fixture;\n\
             public final class Fields {\n\
                 public static final Key<Payload> PAYLOAD = new Key<>(new Payload());\n\
             }\n"
            .into(),
        ),
    ];
    let Some((classes, _)) = common::javac_compile(&sources, &[]) else {
        return;
    };
    let root = classes.parent().map(std::path::Path::to_path_buf);

    let source = "import fixture.Fields\n\
                  fun box(): String {\n\
                      return Fields.PAYLOAD.value().message()\n\
                  }\n";
    let classpath = [classes, stdlib];
    let result = common::compile_and_run_box(source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            let diagnostics =
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()));
            panic!("compile/run failed: {diagnostics:?}");
        });
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(result, "OK");
}
