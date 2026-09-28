//! Every call common lowering builds names the declaration the frontend selected. A `super` call
//! names its function, or for an accessor its property. A backend finds the declaration or the
//! property's own realization from these identities; it never derives a property from an
//! accessor's spelling.

use super::tests::lower_single_source;
use crate::fir::{CallableId, PropertyId, ResolvedFunctionOverrideTarget};
use crate::ir::{Callee, IrExpr, IrFile, IrLocalPropertyLayout, IrSuperCallKind};
use crate::types::{type_name, Ty};

/// The `(kind, declaration)` of every `super` call in the file, in expression order.
fn super_calls(ir: &IrFile) -> Vec<(IrSuperCallKind, Option<ResolvedFunctionOverrideTarget>)> {
    ir.exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call {
                callee: Callee::Super {
                    kind, declaration, ..
                },
                ..
            } => Some((*kind, *declaration)),
            _ => None,
        })
        .collect()
}

/// The checked callable of the member `name(parameters)` that `owner` declares in this file.
fn member(ir: &IrFile, owner: &str, name: &str, parameters: &[Ty]) -> CallableId {
    let owner = type_name(owner);
    let matches = ir
        .checked_callable_functions
        .iter()
        .filter(|(_, function)| {
            let function = &ir.functions[**function as usize];
            function.dispatch_receiver == Some(owner)
                && function.name == name
                && function.params == parameters
        })
        .map(|(callable, _)| *callable)
        .collect::<Vec<_>>();
    let [callable] = matches[..] else {
        panic!("one {name}{parameters:?} in {owner:?}, found {matches:?}")
    };
    callable
}

/// The property `name` that the class `owner` declares in this file.
fn member_property(ir: &IrFile, owner: &str, name: &str) -> PropertyId {
    let owner = type_name(owner);
    let matches = ir
        .local_property_layouts
        .iter()
        .filter(|(_, layout)| {
            matches!(
                layout,
                IrLocalPropertyLayout::Member {
                    owner: declared,
                    name: declared_name,
                    ..
                } if *declared == owner && declared_name == name
            )
        })
        .map(|(property, _)| *property)
        .collect::<Vec<_>>();
    let [property] = matches[..] else {
        panic!("one property {name} in {owner:?}, found {matches:?}")
    };
    property
}

#[test]
fn a_super_accessor_names_its_property_and_a_super_call_its_declaration() {
    let ir = lower_single_source(
        "open class Base {
    open var level: Int = 1
    open fun rank(x: Int): Int = x
    open fun rank(x: String): Int = x.length
}
class Other {
    var level: Int = 2
}
class Derived : Base() {
    override var level: Int
        get() = super.level + 1
        set(value) {
            super.level = value
        }
    override fun rank(x: Int): Int = super.rank(x) + 1
}
",
        "Supers",
    );
    let level = member_property(&ir, "Base", "level");
    assert_ne!(level, member_property(&ir, "Other", "level"));
    let rank = member(&ir, "Base", "rank", &[Ty::Int]);
    let mut actual = super_calls(&ir);
    let mut expected = vec![
        // An accessor has no callable of its own: its kind names its property.
        (IrSuperCallKind::PropertyGetter(level), None),
        (IrSuperCallKind::PropertySetter(level), None),
        (
            IrSuperCallKind::Function,
            Some(ResolvedFunctionOverrideTarget::Module(rank)),
        ),
    ];
    let key =
        |call: &(IrSuperCallKind, Option<ResolvedFunctionOverrideTarget>)| format!("{call:?}");
    actual.sort_by_key(key);
    expected.sort_by_key(key);
    assert_eq!(actual, expected);
}
