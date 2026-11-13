use super::*;
use krusty::types::type_name;

fn names(package: &str, simple: Option<&str>, qualified: Option<&str>) -> ReflectionNames {
    ReflectionNames {
        package: package.into(),
        simple: simple.map(Into::into),
        qualified: qualified.map(Into::into),
    }
}

#[test]
fn a_token_renders_kotlin_wasm_names_for_each_declaration_kind() {
    assert_eq!(
        rendered_name(&names(
            "foo.test",
            Some("Nested"),
            Some("foo.test.Outer.Nested")
        )),
        "foo.test.Outer.Nested"
    );
    assert_eq!(
        rendered_name(&names("foo.test", Some("Local"), None)),
        "foo.test.Local"
    );
    assert_eq!(
        rendered_name(&names("foo.test", None, None)),
        "foo.test.<no name provided>"
    );
    assert_eq!(rendered_name(&names("", Some("Local"), None)), "Local");
    assert_eq!(rendered_name(&names("", None, None)), "<no name provided>");
}

#[test]
fn one_classifier_has_one_record_named_once() {
    let mut module = Module::default();
    let (chars, string) = crate::runtime::Runtime::declare_types(&mut module);
    let mut infos = TypeInfos::declare(&mut module, chars, string);
    let first = type_name("demo/First");
    let second = type_name("demo/Second");
    let reserved = infos.global(&mut module, first);
    assert_eq!(infos.global(&mut module, second), reserved + 1);
    assert_eq!(infos.unnamed(), [first, second]);

    infos.name(
        &mut module,
        second,
        &names("demo", Some("Second"), Some("demo.Second")),
    );
    assert_eq!(infos.unnamed(), [first]);
    let named = module.encode();
    // A later file names the same classifier from the same facts: nothing changes.
    infos.name(
        &mut module,
        second,
        &names("demo", Some("Other"), Some("demo.Other")),
    );
    assert_eq!(module.encode(), named);
    assert_eq!(infos.global(&mut module, first), reserved);
}
