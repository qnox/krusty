use super::{source_ordered_members, SourceOrderedMember};
use krusty::ir::{IrFile, IrFunction};
use krusty::types::Ty;

fn method(ir: &mut IrFile, class: u32, name: &str) -> u32 {
    let owner = ir.classes[class as usize].fq_name_id();
    let function = ir.add_fun(IrFunction {
        name: name.to_owned(),
        params: Vec::new(),
        ret: Ty::String,
        body: None,
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: Vec::new(),
    });
    ir.classes[class as usize].methods.push(function);
    function
}

/// kotlinc generates an `interface by` forwarder after the class's declarations and after the
/// structural members it synthesizes itself, such as a value class's `toString`, `hashCode` and
/// `equals`, even when those are added to the class later than the forwarder.
#[test]
fn delegation_forwarders_follow_members_generated_later() {
    let mut ir = IrFile::default();
    let class = ir.add_class(krusty::plugins::synthetic_class("demo/Wrapper"));
    let forwarder = method(&mut ir, class, "greet");
    ir.interface_delegation_forwarders.insert(forwarder);
    let declared = method(&mut ir, class, "own");
    ir.fn_source_order.insert(declared, 0);
    let generated = method(&mut ir, class, "toString");

    let order: Vec<u32> = source_ordered_members(&ir, &ir.classes[class as usize], None)
        .into_iter()
        .map(|member| match member {
            SourceOrderedMember::Function(function) => function,
            _ => panic!("only functions are scheduled"),
        })
        .collect();
    assert_eq!(order, [declared, generated, forwarder]);
}

/// kotlinc's serialization plugin adds `write$Self` in its IR phase, after the frontend declared
/// the class's `interface by` forwarders, and emits the deserialization constructor after both.
#[test]
fn ir_plugin_members_follow_delegation_forwarders() {
    let mut ir = IrFile::default();
    let class = ir.add_class(krusty::plugins::synthetic_class("demo/Named"));
    let owner = ir.classes[class as usize].fq_name_id();
    let write_self = method(&mut ir, class, "write$Self$main");
    ir.record_generated_function(
        owner,
        krusty::ir::IrGeneratedFunctionRole::SerializationWriteSelf,
        write_self,
    );
    let forwarder = method(&mut ir, class, "greet");
    ir.interface_delegation_forwarders.insert(forwarder);
    let declared = method(&mut ir, class, "own");
    ir.fn_source_order.insert(declared, 0);

    let order: Vec<u32> = source_ordered_members(&ir, &ir.classes[class as usize], None)
        .into_iter()
        .map(|member| match member {
            SourceOrderedMember::Function(function) => function,
            _ => panic!("only functions are scheduled"),
        })
        .collect();
    assert_eq!(order, [declared, forwarder, write_self]);
}
