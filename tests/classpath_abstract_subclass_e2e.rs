use super::common;

#[test]
fn named_and_anonymous_classes_extend_an_abstract_classpath_class() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(libout) = common::compile_lib(
        "absbase",
        "package lib\n\
         abstract class Greeter {\n\
         \x20 fun greet(name: String): String = \"hi \" + name\n\
         }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Greeter\n\
        class NamedGreeter : Greeter()\n\
        fun box(): String {\n\
        \x20 val anonymous: Greeter = object : Greeter() {}\n\
        \x20 val named: Greeter = NamedGreeter()\n\
        \x20 return if (anonymous.greet(\"anonymous\") == \"hi anonymous\" && named.greet(\"named\") == \"hi named\") \"OK\" else \"fail\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to subclass an abstract classpath class");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn unsafe_abstract_classpath_bases_are_declined() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(libout) = common::compile_lib(
        "unsafeabsbase",
        "package lib\n\
         abstract class RequiresOverride { abstract fun value(): String }\n\
         abstract class Closed private constructor()\n",
    ) else {
        return;
    };
    let cp = vec![libout, sl];

    let implements_abstract =
        "import lib.RequiresOverride\nclass Child : RequiresOverride() { override fun value() = \"x\" }\n";
    assert!(
        common::compile_in_process(implements_abstract, "Override", &cp, Some(jdk.as_path()))
            .is_none()
    );

    let inaccessible_constructor = "import lib.Closed\nclass Child : Closed()\n";
    assert!(common::compile_in_process(
        inaccessible_constructor,
        "Closed",
        &cp,
        Some(jdk.as_path())
    )
    .is_none());
}
