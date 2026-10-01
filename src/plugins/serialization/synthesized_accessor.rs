//! A generated getter call bound to the property that owns its backing field.
//!
//! Serialization emits `invokevirtual Owner.getName()` for a field it did not lower itself.
//! Only a field that is a property has a synthesized accessor. The value-class pass renames
//! that call only through this binding, and retires the binding when it replaces the call.

use crate::ir::{Callee, ClassId, ExprId, IrExpr, IrFile, SynthesizedAccessorCall};
use crate::names::property_getter_name;
use crate::types::TypeName;

/// Emit a virtual property getter and bind it to the property that owns `field` when one exists.
pub(super) fn getter(
    ir: &mut IrFile,
    receiver: ExprId,
    owner: TypeName,
    property_name: &str,
    descriptor: String,
    property: (ClassId, u32),
) -> ExprId {
    let call = ir.add_expr(IrExpr::Call {
        callee: Callee::Virtual {
            owner,
            name: property_getter_name(property_name),
            descriptor: format!("(){descriptor}"),
            params: None,
            interface: false,
            module_target: None,
        },
        dispatch_receiver: Some(receiver),
        args: vec![],
    });
    bind_field_getter(ir, call, property.0, property.1);
    call
}

/// Record that `call` is the synthesized getter of the property whose backing field is `field`.
pub(super) fn bind_synthesized_getter(ir: &mut IrFile, call: ExprId, class: ClassId, field: u32) {
    let property = ir.classes[class as usize]
        .properties
        .iter()
        .position(|property| property.backing_field == Some(field))
        .unwrap_or_else(|| {
            panic!(
                "synthesized getter call {call} on class {class} names field {field}, which is not a property"
            )
        });
    ir.synthesized_accessor_calls.insert(
        call,
        SynthesizedAccessorCall {
            class,
            property: property as u32,
        },
    );
}

/// Publish [`bind_synthesized_getter`] when `field` is a property.
///
/// A serialized field with no property is an ordinary getter. Its plain spelling is the call's
/// name, so no accessor binding is published.
pub(super) fn bind_field_getter(ir: &mut IrFile, call: ExprId, class: ClassId, field: u32) {
    let is_property = ir.classes[class as usize]
        .properties
        .iter()
        .any(|property| property.backing_field == Some(field));
    if is_property {
        bind_synthesized_getter(ir, call, class, field);
    }
}

#[cfg(test)]
mod tests {
    use super::{bind_field_getter, bind_synthesized_getter};
    use crate::ir::{IrClass, IrConst, IrExpr, IrFile, IrProperty};
    use crate::types::{type_name, Ty, Visibility};

    fn property(field: u32) -> IrProperty {
        IrProperty {
            name: "result".to_string(),
            context_params: Vec::new(),
            source_order: 0,
            decl_line: 0,
            ty: Ty::obj("kotlin/Result"),
            type_params: Vec::new(),
            visibility: Visibility::Public,
            return_value_status: Default::default(),
            annotations: Box::new([]),
            initializer: None,
            storage_ty: None,
            backing_field: Some(field),
            is_var: false,
            is_open: false,
            modifiers: Default::default(),
            delegate_field: None,
            is_private: false,
            setter_visibility: Visibility::Public,
            getter: None,
            setter: None,
            getter_jvm_name: None,
            setter_jvm_name: None,
            needs_access_bridge: false,
        }
    }

    #[test]
    fn a_backing_field_binds_the_call_to_that_property() {
        let mut ir = IrFile::default();
        let class = ir.add_class(IrClass::synthetic(type_name("sample/Foo")));
        ir.classes[class as usize].properties.push(property(0));
        let call = ir.add_expr(IrExpr::Const(IrConst::Null));
        bind_synthesized_getter(&mut ir, call, class, 0);
        let binding = &ir.synthesized_accessor_calls[&call];
        assert_eq!(binding.class, class);
        assert_eq!(binding.property, 0);
    }

    #[test]
    fn a_field_with_no_property_keeps_the_plain_getter() {
        let mut ir = IrFile::default();
        let class = ir.add_class(IrClass::synthetic(type_name("sample/Foo")));
        let call = ir.add_expr(IrExpr::Const(IrConst::Null));
        bind_field_getter(&mut ir, call, class, 0);
        assert!(
            !ir.synthesized_accessor_calls.contains_key(&call),
            "a field that is not a property is not a synthesized accessor"
        );
    }

    #[test]
    #[should_panic(expected = "names field 1, which is not a property")]
    fn a_missing_backing_field_property_is_not_left_unbound() {
        let mut ir = IrFile::default();
        let class = ir.add_class(IrClass::synthetic(type_name("sample/Foo")));
        ir.classes[class as usize].properties.push(property(0));
        let call = ir.add_expr(IrExpr::Const(IrConst::Null));
        bind_synthesized_getter(&mut ir, call, class, 1);
    }
}
