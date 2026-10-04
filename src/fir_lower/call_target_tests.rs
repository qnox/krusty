//! Every call common lowering builds names the declaration the frontend selected. A virtual call
//! names its member, and a `super` call its function, or for an accessor its property. A backend
//! finds a dispatch slot or a property's own realization from these identities; it never matches a
//! member's name and parameters, or derives a property from an accessor's spelling.

use super::tests::{lower_single_source, lower_source_from_set};
use crate::fir::{CallableId, PropertyId, ResolvedFunctionOverrideTarget};
use crate::ir::{Callee, IrExpr, IrFile, IrLocalPropertyLayout, IrSuperCallKind, IrVirtualTarget};
use crate::types::{type_name, Ty};

/// The `(name, target)` of every virtual call in the file, sorted by name.
fn virtual_targets(ir: &IrFile) -> Vec<(String, Option<IrVirtualTarget>)> {
    let mut targets = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call {
                callee: Callee::Virtual { name, target, .. },
                ..
            } => Some((name.clone(), *target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| left.0.cmp(&right.0));
    targets
}

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

fn function(callable: CallableId) -> Option<IrVirtualTarget> {
    Some(IrVirtualTarget::Function(
        ResolvedFunctionOverrideTarget::Module(callable),
    ))
}

/// `Box.take` is overloaded, and an unrelated `Other.take(Int)` has the same name and shape.
const BOXES: &str = "open class Box {
    fun take(x: Int): Int = x
    fun take(x: String): Int = x.length
}
class Other {
    fun take(x: Int): Int = x
}
";

#[test]
fn a_call_to_a_sibling_file_member_names_the_selected_overload() {
    let sources = [
        (BOXES, "Boxes"),
        (
            "fun useInt(box: Box): Int = box.take(2)\nfun useText(box: Box): Int = box.take(\"t\")\n",
            "Use",
        ),
    ];
    let declaring = lower_source_from_set(&sources, 0);
    let calling = lower_source_from_set(&sources, 1);
    let by_int = member(&declaring, "Box", "take", &[Ty::Int]);
    let by_text = member(&declaring, "Box", "take", &[Ty::String]);
    assert_ne!(by_int, member(&declaring, "Other", "take", &[Ty::Int]));
    let mut expected = vec![
        ("take".to_owned(), function(by_int)),
        ("take".to_owned(), function(by_text)),
    ];
    let mut actual = virtual_targets(&calling);
    let key = |target: &(String, Option<IrVirtualTarget>)| format!("{target:?}");
    expected.sort_by_key(key);
    actual.sort_by_key(key);
    assert_eq!(actual, expected);
}

#[test]
fn an_interface_delegation_forwards_to_each_selected_member_and_accessor() {
    let ir = lower_single_source(
        "interface Source {
    val size: Int
    var cursor: Int
    fun read(at: Int): Int
}
class Other {
    fun read(at: Int): Int = at
}
class Forwarding(source: Source) : Source by source
",
        "Delegation",
    );
    let size = ir
        .property_overrides
        .values()
        .flatten()
        .find_map(|edge| match edge.overridden {
            crate::fir::ResolvedPropertyOverrideTarget::Module(property) if edge.name == "size" => {
                Some(property)
            }
            _ => None,
        })
        .expect("Forwarding.size overrides Source.size");
    let cursor = ir
        .property_overrides
        .values()
        .flatten()
        .find_map(|edge| match edge.overridden {
            crate::fir::ResolvedPropertyOverrideTarget::Module(property)
                if edge.name == "cursor" =>
            {
                Some(property)
            }
            _ => None,
        })
        .expect("Forwarding.cursor overrides Source.cursor");
    let read = member(&ir, "Source", "read", &[Ty::Int]);
    assert_ne!(read, member(&ir, "Other", "read", &[Ty::Int]));
    assert_eq!(
        virtual_targets(&ir),
        [
            (
                "getCursor".to_owned(),
                Some(IrVirtualTarget::PropertyGetter(
                    crate::fir::ResolvedPropertyOverrideTarget::Module(cursor),
                ))
            ),
            (
                "getSize".to_owned(),
                Some(IrVirtualTarget::PropertyGetter(
                    crate::fir::ResolvedPropertyOverrideTarget::Module(size),
                ))
            ),
            ("read".to_owned(), function(read)),
            (
                "setCursor".to_owned(),
                Some(IrVirtualTarget::PropertySetter(
                    crate::fir::ResolvedPropertyOverrideTarget::Module(cursor),
                ))
            ),
        ]
    );
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

#[test]
fn a_delegate_convention_on_a_sibling_file_class_names_the_selected_operator() {
    let sources = [
        (
            "class Holder {
    operator fun getValue(owner: Any?, property: Any?): Int = 1
    fun getValue(owner: Any?): Int = 2
}
class Other {
    operator fun getValue(owner: Any?, property: Any?): Int = 3
}
",
            "Holders",
        ),
        ("val held: Int by Holder()\n", "Delegated"),
    ];
    let declaring = lower_source_from_set(&sources, 0);
    let delegated = lower_source_from_set(&sources, 1);
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let convention = member(&declaring, "Holder", "getValue", &[any, any]);
    assert_ne!(convention, member(&declaring, "Holder", "getValue", &[any]));
    assert_ne!(
        convention,
        member(&declaring, "Other", "getValue", &[any, any])
    );
    assert_eq!(
        virtual_targets(&delegated),
        [("getValue".to_owned(), function(convention))]
    );
}
