//! The shared accessor realization (`backend::local_properties`) over lowered common IR: a
//! member-extension property accessor call realized from a checked property access names the
//! property the checker selected, so a backend reaches the accessor without matching its spelling.

use crate::fir::PropertyId;
use crate::ir::{Callee, IrExpr, IrFile, IrLocalPropertyLayout, IrVirtualTarget};

/// The member-extension property `name` that the class `owner` declares in this file.
fn member_extension(ir: &IrFile, owner: &str, name: &str) -> PropertyId {
    let owner = crate::types::type_name(owner);
    let matches = ir
        .local_property_layouts
        .iter()
        .filter(|(_, layout)| {
            matches!(
                layout,
                IrLocalPropertyLayout::MemberExtension {
                    owner: declared_owner,
                    name: declared,
                    ..
                } if *declared_owner == owner && declared == name
            )
        })
        .map(|(property, _)| *property)
        .collect::<Vec<_>>();
    let [property] = matches[..] else {
        panic!("one member-extension property {name}, found {matches:?}")
    };
    property
}

#[test]
fn a_member_extension_accessor_call_names_its_property() {
    let mut ir = crate::fir_lower::tests::lower_single_source_with_platform(
        "class Host {
    val Int.twice: Int
        get() = this * 2
    var Int.slot: Int
        get() = this
        set(value) {}
    fun read(x: Int): Int = x.twice + x.slot
    fun write(x: Int) {
        x.slot = 4
    }
}
class Other {
    val Int.twice: Int
        get() = this * 3
}
",
        "MemberExtensions",
        Box::new(crate::libraries::EmptySymbolSource),
    );
    crate::backend::local_properties::realize(&mut ir)
        .expect("every checked property access has a layout");
    let twice = member_extension(&ir, "Host", "twice");
    assert_ne!(twice, member_extension(&ir, "Other", "twice"));
    let slot = member_extension(&ir, "Host", "slot");
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
    assert_eq!(
        targets,
        [
            (
                "getSlot".to_owned(),
                Some(IrVirtualTarget::PropertyGetter(slot))
            ),
            (
                "getTwice".to_owned(),
                Some(IrVirtualTarget::PropertyGetter(twice))
            ),
            (
                "setSlot".to_owned(),
                Some(IrVirtualTarget::PropertySetter(slot))
            ),
        ]
    );
}
