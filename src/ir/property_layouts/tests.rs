use super::*;
use crate::fir::{CallableId, DeclarationId, PropertyId};
use crate::ir::{
    IrCheckedOperation, IrClass, IrConst, IrExpr, IrField, IrFile, IrFunction, IrProperty,
};

const TARGET: PropertyId = PropertyId::from_raw(7);

fn fixture() -> (IrFile, u32) {
    let mut ir = IrFile::default();
    let mut class = IrClass::synthetic("p/Declaration".into());
    class.is_anonymous_object = true;
    let mut property = IrProperty::stored_value("value".into(), Ty::String, 0);
    property.is_var = true;
    class.properties.push(property);
    class.fields.push(IrField::new("value".into(), Ty::String));
    let source = ir.add_class(class);
    ir.local_property_layouts.insert(
        TARGET,
        IrLocalPropertyLayout::Member {
            class: source,
            owner: "p/Declaration".into(),
            backing_field: Some(0),
            getter: None,
            setter: None,
            interface: false,
            name: "value".into(),
            ty: Ty::String,
            mutable: true,
            private: false,
            context_parameters: Vec::new(),
            property: 0,
        },
    );
    let receiver = ir.add_expr(IrExpr::Const(IrConst::Null));
    let read = ir.add_expr(IrExpr::Checked(Box::new(
        IrCheckedOperation::PropertyRead {
            target: TARGET,
            dispatch_receiver: Some(receiver),
            extension_receiver: None,
            context_arguments: Vec::new(),
            substitutions: Vec::new(),
        },
    )));
    (ir, read)
}

fn copy(ir: &mut IrFile, source: u32, name: &str, field: u32) -> u32 {
    let mut class = ir.classes[source as usize].clone();
    let field_count = class.fields.len() as u32;
    let source_getter = class.properties[0].getter;
    let source_setter = class.properties[0].setter;
    class.fq_name = name.into();
    while class.fields.len() <= field as usize {
        class.fields.push(IrField::new(
            format!("storage{}", class.fields.len()),
            Ty::String,
        ));
    }
    let body = ir.add_expr(IrExpr::Const(IrConst::String("value".into())));
    let getter = ir.add_fun(IrFunction {
        name: "getValue".into(),
        params: Vec::new(),
        ret: Ty::String,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(name.into()),
        param_checks: Vec::new(),
    });
    let body = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let setter = ir.add_fun(IrFunction {
        name: "setValue".into(),
        params: vec![Ty::String],
        ret: Ty::Unit,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(name.into()),
        param_checks: Vec::new(),
    });
    let method_clones = source_getter
        .map(|source| (source, getter))
        .into_iter()
        .chain(source_setter.map(|source| (source, setter)))
        .collect();
    class.properties[0].backing_field = Some(field);
    class.properties[0].getter = Some(getter);
    class.properties[0].setter = Some(setter);
    class.methods = vec![getter, setter];
    let id = ir.add_class(class);
    ir.specialized_anonymous_classes.insert(
        id,
        crate::ir::IrSpecializedAnonymousClass {
            source,
            order: 0,
            caller_declaration: DeclarationId::from_raw(1),
            caller: None,
            caller_is_default: false,
            caller_source_name: "caller".into(),
            inline_callee: CallableId::from_raw(0),
            inline_callee_source_name: "callee".into(),
            crossinline_operands: Vec::new(),
            method_clones,
            bindings: Default::default(),
            reified_bindings: Default::default(),
            field_count,
            property_count: 1,
            omitted_capture_fields: 0,
            caller_property_uses: Vec::new(),
            pending_property_roots: Vec::new(),
        },
    );
    id
}

fn member(
    layout: Option<IrLocalPropertyLayout>,
) -> (u32, u32, Option<u32>, Option<u32>, Option<u32>) {
    let Some(IrLocalPropertyLayout::Member {
        class,
        property,
        backing_field,
        getter,
        setter,
        ..
    }) = layout
    else {
        panic!("the selected member layout");
    };
    (class, property, backing_field, getter, setter)
}

#[test]
fn two_call_sites_share_the_declaration_but_keep_their_own_storage_and_accessors() {
    let (mut ir, read) = fixture();
    let a = copy(&mut ir, 0, "p/First", 2);
    let b = copy(&mut ir, 0, "p/Second", 4);
    let second = ir.add_expr(ir.expr(read).clone());
    ir.dispatch_classes
        .insert(read, ir.classes[a as usize].fq_name);
    ir.dispatch_classes
        .insert(second, ir.classes[b as usize].fq_name);
    assert_eq!(
        member(ir.property_layout_at(read, TARGET).unwrap()),
        (a, 0, Some(2), Some(0), Some(1))
    );
    assert_eq!(
        member(ir.property_layout_at(second, TARGET).unwrap()),
        (b, 0, Some(4), Some(2), Some(3))
    );
    assert_eq!(
        member(ir.local_property_layouts.get(&TARGET).cloned()),
        (0, 0, Some(0), None, None)
    );
    let value = ir.add_expr(IrExpr::Const(IrConst::String("changed".into())));
    let write = ir.add_expr(IrExpr::Checked(Box::new(
        IrCheckedOperation::PropertyWrite {
            target: TARGET,
            dispatch_receiver: Some(0),
            extension_receiver: None,
            context_arguments: Vec::new(),
            value,
            substitutions: Vec::new(),
        },
    )));
    ir.dispatch_classes
        .insert(write, ir.classes[b as usize].fq_name);
    let realized = crate::backend::local_properties::realize(&mut ir).unwrap();
    assert_eq!(
        realized
            .iter()
            .map(|access| access.target)
            .collect::<Vec<_>>(),
        vec![TARGET, TARGET, TARGET]
    );
    let IrExpr::PropertyWrite {
        owner,
        value: actual_value,
        ..
    } = ir.expr(write)
    else {
        panic!("the semantic property write remains a write");
    };
    assert_eq!(*owner, ir.classes[b as usize].fq_name);
    assert_eq!(*actual_value, value);
    for (expression, owner) in [(read, a), (second, b)] {
        let IrExpr::PropertyRead {
            owner: actual,
            operation,
            ..
        } = ir.expr(expression)
        else {
            panic!("the semantic property read remains a read");
        };
        assert_eq!(*actual, ir.classes[owner as usize].fq_name);
        assert_eq!(*operation, Some(expression));
    }
}

#[test]
fn a_copy_of_a_copy_keeps_the_original_property_identity() {
    let (mut ir, read) = fixture();
    let first = copy(&mut ir, 0, "p/First", 2);
    let final_copy = copy(&mut ir, first, "p/Final", 5);
    ir.dispatch_classes
        .insert(read, ir.classes[final_copy as usize].fq_name);
    assert_eq!(
        member(ir.property_layout_at(read, TARGET).unwrap()),
        (final_copy, 0, Some(5), Some(2), Some(3))
    );
}

#[test]
fn missing_or_cyclic_copy_layouts_fail_without_changing_the_operation() {
    for cycle in [false, true] {
        let (mut ir, read) = fixture();
        let copy = copy(&mut ir, 0, "p/Copy", 2);
        ir.dispatch_classes
            .insert(read, ir.classes[copy as usize].fq_name);
        if cycle {
            ir.specialized_anonymous_classes
                .get_mut(&copy)
                .unwrap()
                .source = copy;
        } else {
            ir.classes[copy as usize].properties.clear();
        }
        assert!(matches!(ir.property_layout_at(read, TARGET), Err(target) if target == TARGET));
        assert!(
            matches!(crate::backend::local_properties::realize(&mut ir), Err(target) if target == TARGET)
        );
        assert!(matches!(ir.expr(read), IrExpr::Checked(_)));
    }
}

#[test]
fn an_ordinary_inherited_property_keeps_its_declaration_layout() {
    let (mut ir, read) = fixture();
    ir.classes[0].is_anonymous_object = false;
    let mut child = IrClass::synthetic("p/OrdinaryChild".into());
    child.superclass = "p/Declaration".into();
    let child = ir.add_class(child);
    ir.dispatch_classes
        .insert(read, ir.classes[child as usize].fq_name);
    assert_eq!(
        member(ir.property_layout_at(read, TARGET).unwrap()),
        (0, 0, Some(0), None, None)
    );
}

#[test]
fn an_explicit_unknown_or_unrelated_specialization_cannot_use_the_source_layout() {
    for unrelated in [false, true] {
        let (mut ir, read) = fixture();
        let owner = if unrelated {
            let mut other = ir.classes[0].clone();
            other.fq_name = "p/OtherDeclaration".into();
            let other = ir.add_class(other);
            let copied = copy(&mut ir, other, "p/UnrelatedCopy", 2);
            ir.classes[copied as usize].fq_name
        } else {
            "p/MissingCopy".into()
        };
        ir.dispatch_classes.insert(read, owner);
        assert!(matches!(ir.property_layout_at(read, TARGET), Err(target) if target == TARGET));
        assert!(matches!(ir.expr(read), IrExpr::Checked(_)));
    }
}

#[test]
fn a_copied_anonymous_class_keeps_its_inherited_storage_and_accessors_on_the_base() {
    for interface in [false, true] {
        let (mut ir, read) = fixture();
        ir.classes[0].is_anonymous_object = false;
        ir.classes[0].is_interface = interface;
        let body = ir.add_expr(IrExpr::Const(IrConst::String("base".into())));
        let getter = ir.add_fun(IrFunction {
            name: "getValue".into(),
            params: Vec::new(),
            ret: Ty::String,
            body: Some(body),
            is_static: false,
            dispatch_receiver: Some("p/Declaration".into()),
            param_checks: Vec::new(),
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        });
        let setter = ir.add_fun(IrFunction {
            name: "setValue".into(),
            params: vec![Ty::String],
            ret: Ty::Unit,
            body: Some(body),
            is_static: false,
            dispatch_receiver: Some("p/Declaration".into()),
            param_checks: Vec::new(),
        });
        ir.classes[0].properties[0].getter = Some(getter);
        ir.classes[0].properties[0].setter = Some(setter);
        ir.classes[0].methods = vec![getter, setter];
        if interface {
            ir.classes[0].properties[0].backing_field = None;
            ir.classes[0].fields.clear();
        }
        let Some(IrLocalPropertyLayout::Member {
            getter: selected_getter,
            setter: selected_setter,
            backing_field,
            interface: selected_interface,
            ..
        }) = ir.local_property_layouts.get_mut(&TARGET)
        else {
            panic!("the selected member");
        };
        *selected_getter = Some(getter);
        *selected_setter = Some(setter);
        *selected_interface = interface;
        if interface {
            *backing_field = None;
        }
        let mut anonymous = IrClass::synthetic("p/Anonymous".into());
        anonymous.is_anonymous_object = true;
        if interface {
            anonymous.interfaces = vec!["p/Declaration".into()].into();
        } else {
            anonymous.superclass = "p/Declaration".into();
        }
        // The copy has its own DIFFERENT property at ordinal zero. The inherited selected
        // PropertyId must retain the base layout, not project onto this unrelated slot.
        let mut own = IrProperty::stored_value("own".into(), Ty::String, 0);
        own.is_var = true;
        anonymous.properties.push(own);
        anonymous
            .fields
            .push(IrField::new("own".into(), Ty::String));
        let source = ir.add_class(anonymous);
        let copied = copy(&mut ir, source, "p/CopiedAnonymous", 3);
        let expected = (0, 0, (!interface).then_some(0), Some(getter), Some(setter));
        assert_eq!(
            member(ir.property_layout_for_class(TARGET, copied).unwrap()),
            expected
        );
        ir.dispatch_classes
            .insert(read, ir.classes[copied as usize].fq_name);
        assert_eq!(
            member(ir.property_layout_at(read, TARGET).unwrap()),
            expected
        );
        crate::backend::local_properties::realize(&mut ir).unwrap();
        let IrExpr::PropertyRead { owner, .. } = ir.expr(read) else {
            panic!("the inherited read");
        };
        assert_eq!(*owner, ir.classes[0].fq_name);
    }
}
