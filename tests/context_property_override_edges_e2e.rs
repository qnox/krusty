//! A class's context-parameter property is a candidate implementation of its supertypes'
//! properties, like every other property declaration: common IR records the override edge that a
//! backend dispatches an interface call through.

use super::common;

const SOURCE: &str = "// LANGUAGE: +ContextParameters
interface Cell {
    context(scale: Int)
    var amount: Int
}

class Counter : Cell {
    private var stored = 0
    context(scale: Int)
    override var amount: Int
        get() = stored * scale
        set(value) { stored = value / scale }
}
";

#[test]
fn a_context_property_overriding_an_interface_property_records_its_edge() {
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let (files, diagnostics) = common::capture_common_ir(SOURCE, "ContextCell", platform);
    assert_eq!(diagnostics, Vec::<String>::new());
    let file = files.into_iter().next().expect("one lowered source file");
    let edges = file
        .property_overrides
        .iter()
        .filter(|(class, _)| class.matches("Counter"))
        .flat_map(|(_, edges)| edges)
        .map(|edge| (edge.name.as_str(), edge.overridden_is_interface))
        .collect::<Vec<_>>();
    assert_eq!(edges, vec![("amount", true)]);
}
