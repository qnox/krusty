use super::common;

#[test]
fn byte_array_return_flows_into_constructor() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let java = [
        (
            "Bytes.java".into(),
            r#"
                package fixtures;
                public final class Bytes {
                    public static byte[] create(int size) { return new byte[size]; }
                }
            "#
            .into(),
        ),
        (
            "Packet.java".into(),
            r#"
                package fixtures;
                public final class Packet {
                    private final byte[] bytes;
                    public Packet(byte[] bytes) { this.bytes = bytes; }
                    public int size() { return bytes.length; }
                }
            "#
            .into(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![library, stdlib];
    let source = "import fixtures.Bytes\n\
        import fixtures.Packet\n\
        fun box(): String {\n\
        \x20 val payload: ByteArray = Bytes.create(2)\n\
        \x20 val packet = Packet(payload)\n\
        \x20 return if (payload.size == 2 && packet.size() == 2) \"OK\" else \"fail\"\n\
        }\n";
    let classes = common::compile_in_process(source, "Main", &classpath, Some(&jdk))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(source, &classpath, Some(&jdk))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}
