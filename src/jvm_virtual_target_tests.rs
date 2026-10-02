//! The JVM realization passes keep the member the frontend selected on every virtual call they
//! realize from a checked call: a current-module property accessor on a class in another file,
//! and an ordinary dependency member dispatch. Only a call a pass synthesizes for itself names no
//! selection.

use std::rc::Rc;

use crate::fir::{PropertyId, ResolvedFunctionOverrideTarget};
use crate::ir::{Callee, IrExpr, IrFile, IrVirtualTarget};
use crate::jvm::classpath::Classpath;
use crate::jvm::realization_test_support::realize_calls;
use crate::types::{type_name, TypeName};

/// Every realized virtual call as `(owner, name, target)`, sorted.
fn virtual_calls(ir: &IrFile) -> Vec<(TypeName, String, Option<IrVirtualTarget>)> {
    let mut calls = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call {
                callee:
                    Callee::Virtual {
                        owner,
                        name,
                        target,
                        ..
                    },
                ..
            } => Some((*owner, name.clone(), *target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    calls.sort_by_key(|call| format!("{call:?}"));
    calls
}

/// The property `name` of the class `owner` that another file declares, as this file's module fact.
fn module_property(ir: &IrFile, owner: &str, name: &str) -> PropertyId {
    let owner = type_name(owner);
    let matches = ir
        .referenced_module_properties
        .iter()
        .filter(|(_, property)| property.owner == Some(owner) && property.name == name)
        .map(|(property, _)| *property)
        .collect::<Vec<_>>();
    let [property] = matches[..] else {
        panic!("one property {name} of {owner:?}, found {matches:?}")
    };
    property
}

#[test]
fn a_member_accessor_of_a_class_in_another_file_keeps_its_property() {
    let sources = [
        (
            "class Box {
    var level: Int = 0
}
class Other {
    var level: Int = 0
}
",
            "Library",
        ),
        (
            "fun use(box: Box, other: Other): Int {
    box.level = 3
    return box.level + other.level
}
",
            "Consumer",
        ),
    ];
    let mut ir = crate::fir_lower::tests::lower_source_from_set(&sources, 1);
    realize_calls(
        &mut ir,
        &["Library", "Consumer"],
        &Classpath::new(Vec::new()),
        &crate::libraries::EmptySymbolSource,
    );
    let level = module_property(&ir, "Box", "level");
    let other = module_property(&ir, "Other", "level");
    assert_ne!(level, other);
    assert_eq!(
        virtual_calls(&ir),
        [
            (
                type_name("Box"),
                "getLevel".to_owned(),
                Some(IrVirtualTarget::PropertyGetter(level))
            ),
            (
                type_name("Box"),
                "setLevel".to_owned(),
                Some(IrVirtualTarget::PropertySetter(level))
            ),
            (
                type_name("Other"),
                "getLevel".to_owned(),
                Some(IrVirtualTarget::PropertyGetter(other))
            ),
        ]
    );
}

#[test]
fn a_dependency_member_dispatch_keeps_the_selected_declaration() {
    let mut paths = Vec::new();
    paths.extend(crate::jvm::kotlin_stdlib_jar());
    paths.extend(crate::jvm::classpath::platform_jdk_modules(None));
    let classpath = Rc::new(Classpath::new(paths));
    let mut ir = crate::fir_lower::tests::lower_single_source_with_platform(
        "fun grow(builder: StringBuilder): StringBuilder = builder.append(\"x\").append(1)
",
        "Dependency",
        Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
                .expect("JVM provider initialization"),
        ),
    );
    // The checked calls name the selected dependency declarations before realization.
    let mut selected = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call {
                callee: Callee::External { target, .. },
                ..
            } => Some(*target),
            _ => None,
        })
        .collect::<Vec<_>>();
    selected.sort();
    let realization_symbols = crate::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
        .expect("JVM provider initialization");
    realize_calls(&mut ir, &["Dependency"], &classpath, &realization_symbols);
    let calls = virtual_calls(&ir);
    let mut realized = calls
        .iter()
        .map(|(owner, name, target)| {
            let Some(IrVirtualTarget::Function(ResolvedFunctionOverrideTarget::External(target))) =
                target
            else {
                panic!("{owner:?}.{name} names no dependency declaration: {target:?}")
            };
            let declaration = classpath.external_callable(*target).expect("realized");
            assert_eq!(
                (
                    declaration.callable.owner,
                    declaration.callable.physical_name()
                ),
                (*owner, name.as_str())
            );
            *target
        })
        .collect::<Vec<_>>();
    realized.sort();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(realized, selected);
    // `append(String)` and `append(Int)` are two declarations.
    assert_ne!(realized[0], realized[1]);
}
