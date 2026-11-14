//! An object that implements two interfaces records each override against the interface that
//! declares it. A single-interface template cannot show an owner taken from the first supertype.

use super::*;
use crate::fir::{
    ExternalCallableId, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget,
};
use crate::ir::{Callee, ExprId, IrExpr};
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};
use crate::symbol_source::SymbolSource;

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_object_overrides/Library.kt");

#[test]
fn two_interface_object_records_each_declaring_owner() {
    let Some(root) = distribution_root() else {
        return;
    };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let work = loop {
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "krusty-klib-object-overrides-{}-{ordinal}",
            std::process::id()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => break directory,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create producer directory: {error}"),
        }
    };
    let source = work.join("Overrides.kt");
    std::fs::write(&source, LIBRARY).unwrap();
    let library = work.join("overrides");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .expect("run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "producer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            KlibStdlibSemantics::Native,
        ),
        KlibLibraryInput::dependency(library),
    ])
    .expect("read the two-interface object library");
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), "make");
    let [function] = symbols.callables.functions() else {
        panic!("one inline producer")
    };
    let selected = libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap();
    let mut ir = IrFile::default();
    let template = lower_inline_template(
        &mut ir,
        &selected,
        libraries.inline_declaration_bodies(),
        TemplateCallees {
            declarations: libraries.declaration_bodies(),
            references: libraries.reference_bindings(),
            source: &libraries,
            type_hierarchy: None,
        },
    )
    .expect("a klib declaration")
    .expect("the two-interface template lowers");
    validated(&ir);

    let first = type_name("fixture/First");
    let second = type_name("fixture/Second");
    let first_id = member(&libraries, "fixture/First", "first");
    let second_id = member(&libraries, "fixture/Second", "second");
    let carried_id = property_getter(&libraries, "fixture/Second", "carried");
    let class = ir
        .classes
        .iter()
        .find(|class| class.is_anonymous_object)
        .expect("the template builds an anonymous object");
    let owner = class.fq_name_id();
    let functions = ir
        .function_overrides
        .get(&owner)
        .expect("the object publishes its function overrides");
    let properties = ir
        .property_overrides
        .get(&owner)
        .expect("the object publishes its property overrides");
    assert_eq!(functions.len(), 2, "{functions:?}");
    assert_eq!(properties.len(), 1, "{properties:?}");
    let parameter = libraries
        .external_callable(second_id)
        .expect("Second.second")
        .klib_body_class_contract
        .expect("Second publishes its class contract")
        .parameters
        .formals
        .to_vec();
    let [parameter] = parameter.as_slice() else {
        panic!("Second has one type parameter, saw {parameter:?}");
    };
    assert!(parameter.is_ty_param());
    let sequence = Ty::obj("kotlin/CharSequence");
    let first_edge = edge(functions, first_id);
    assert_eq!(first_edge.overridden_owner, first);
    assert_eq!(first_edge.implementation, first_edge.overridden);
    assert_eq!(first_edge.declared_parameters, Vec::<Ty>::new());
    assert_eq!(first_edge.declared_result, Ty::String);
    assert_eq!(first_edge.applied_parameters, Vec::<Ty>::new());
    assert_eq!(first_edge.applied_result, Ty::String);
    assert_eq!(first_edge.implementation_parameters, Vec::<Ty>::new());
    assert_eq!(first_edge.implementation_result, Ty::String);
    assert!(first_edge.overridden_is_interface);

    let second_edge = edge(functions, second_id);
    assert_eq!(second_edge.overridden_owner, second);
    assert_eq!(second_edge.implementation, second_edge.overridden);
    assert_eq!(second_edge.declared_parameters, vec![*parameter]);
    assert_eq!(second_edge.declared_result, sequence);
    assert_eq!(second_edge.applied_parameters, vec![Ty::String]);
    assert_eq!(second_edge.applied_result, sequence);
    assert_eq!(second_edge.implementation_parameters, vec![Ty::String]);
    assert_eq!(second_edge.implementation_result, Ty::String);
    assert!(second_edge.overridden_is_interface);

    let carried = properties
        .iter()
        .find(|edge| edge.overridden == ResolvedPropertyOverrideTarget::External(carried_id))
        .expect("the carried property override");
    assert_eq!(
        properties
            .iter()
            .filter(|edge| edge.overridden == ResolvedPropertyOverrideTarget::External(carried_id))
            .count(),
        1
    );
    assert_eq!(carried.overridden_owner, second);
    assert_eq!(carried.implementation, carried.overridden);
    assert_eq!(carried.declared_type, *parameter);
    assert_eq!(carried.applied_type, Ty::String);
    assert_eq!(carried.implementation_type, Ty::String);
    assert!(carried.overridden_is_interface);

    let body = ir.functions[template.function as usize]
        .body
        .expect("the template has a body");
    let calls = external_calls(&ir, body);
    assert!(
        calls.contains(&second_id),
        "the template calls Second.second, saw {calls:?}"
    );
    assert!(
        !calls.contains(&first_id),
        "the template calls through Second, not First"
    );
    std::fs::remove_dir_all(work).unwrap();
}

fn member(libraries: &KlibLibraries, classifier: &str, name: &str) -> ExternalCallableId {
    let record = libraries
        .classifier(type_name(classifier))
        .unwrap_or_else(|| panic!("no classifier {classifier}"));
    let Some(callables) = record.declared_callables.get(name) else {
        panic!(
            "{classifier} declares {:?}, not {name}",
            record.declared_callable_order
        );
    };
    let [function] = callables.functions() else {
        panic!("one {classifier}.{name}")
    };
    function
        .callable
        .external_identity
        .expect("a provider identity")
}

fn property_getter(libraries: &KlibLibraries, classifier: &str, name: &str) -> ExternalCallableId {
    let record = libraries
        .classifier(type_name(classifier))
        .unwrap_or_else(|| panic!("no classifier {classifier}"));
    let Some(callables) = record.declared_callables.get(name) else {
        panic!(
            "{classifier} declares {:?}, not {name}",
            record.declared_callable_order
        );
    };
    let [property] = callables.properties() else {
        panic!("one {classifier}.{name}")
    };
    property
        .getter
        .external_identity
        .expect("the getter has a provider identity")
}

fn edge(
    edges: &[crate::ir::IrFunctionOverride],
    target: ExternalCallableId,
) -> &crate::ir::IrFunctionOverride {
    let matched: Vec<_> = edges
        .iter()
        .filter(|edge| edge.overridden == ResolvedFunctionOverrideTarget::External(target))
        .collect();
    assert_eq!(matched.len(), 1, "{edges:?}");
    matched[0]
}

fn external_calls(ir: &IrFile, expression: ExprId) -> Vec<ExternalCallableId> {
    let mut found = Vec::new();
    let mut pending = vec![expression];
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Call { callee, .. } = ir.expr(expression) {
            if let Callee::External { target, .. } = callee.as_ref() {
                found.push(*target);
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    found
}
