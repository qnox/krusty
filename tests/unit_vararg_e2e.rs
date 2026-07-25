use super::common;
use krusty::jvm::classreader::parse_class;

#[test]
fn unit_vararg_uses_a_reference_array() {
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let src = "class B {\n\
        fun get(vararg x: Unit) = x.size\n\
    }\n\
    fun box(): String = if (B().get(Unit, Unit) == 2) \"OK\" else \"fail\"\n";
    let classes =
        common::compile_in_process(src, "UnitVararg", std::slice::from_ref(&stdlib), Some(&jdk))
            .expect("compile Unit vararg");
    let class = classes
        .iter()
        .find(|(name, _)| name == "B")
        .and_then(|(_, bytes)| parse_class(bytes).ok())
        .expect("parse B");
    assert!(class
        .methods
        .iter()
        .any(|method| method.name == "get" && method.descriptor == "([Lkotlin/Unit;)I"));

    let result = common::run_box(&classes, "UnitVarargKt", &[stdlib]).expect("run box");
    assert_eq!(result, "OK");
}
