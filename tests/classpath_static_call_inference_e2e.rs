use super::common;

#[test]
fn property_type_from_a_classpath_static_call() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let java = [
        (
            "Item.java".into(),
            r#"
                package fixtures;
                public final class Item {
                    private final String value;
                    public Item(String value) { this.value = value; }
                    public String getValue() { return value; }
                }
            "#
            .into(),
        ),
        (
            "Provider.java".into(),
            r#"
                package fixtures;
                public final class Provider {
                    public static Item create(String value) { return new Item(value); }
                    public static Item create(Class<?> type) { return new Item(type.getSimpleName()); }
                }
            "#
            .into(),
        ),
        (
            "LocalProvider.java".into(),
            r#"
                package fixtures;
                public final class LocalProvider {
                    public String create(String value) { return "local:" + value; }
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
    let source = r#"
        import fixtures.LocalProvider
        import fixtures.Provider

        val top = Provider.create("top")
        val qualified = fixtures.Provider.create("qualified")

        class Holder {
            val member = Provider.create("member")
        }

        class LiteralHolder {
            val product = Provider.create(LiteralHolder::class.java)
        }

        class Shadowed {
            val Provider = LocalProvider()
            val member = Provider.create("member")
        }

        fun box(): String {
            if (top.value != "top") return "top"
            if (qualified.value != "qualified") return "qualified"
            if (Holder().member.value != "member") return "member"
            if (LiteralHolder().product.value != "LiteralHolder") return "literal"
            if (Shadowed().member != "local:member") return "shadowed"
            return "OK"
        }
    "#;
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
