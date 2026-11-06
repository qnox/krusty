use crate::ir::{IrClass, IrConst, IrExpr, IrField, IrFile, IrFunction, IrProperty};
use crate::jvm::classreader::parse_class;
use crate::jvm::ir_emit::invariant_tests::emit_for_test;
use crate::jvm::ir_emit::EmitRun;
use crate::types::{type_name, Ty};

#[test]
fn entry_fields_and_accessors_use_their_declared_types_and_source_positions() {
    let mut ir = IrFile::default();
    let owner = type_name("ScheduledEnum");
    let entry = type_name("ScheduledEnum$ENTRY");
    let mut enumeration = IrClass::synthetic(owner);
    enumeration.is_enum = true;
    ir.add_class(enumeration);

    let value = ir.add_expr(IrExpr::Const(IrConst::String("OK".into())));
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    let mut method = |name: &str, order| {
        let function = ir.add_fun(IrFunction {
            name: name.into(),
            params: Vec::new(),
            ret: Ty::String,
            body: Some(returned),
            is_static: false,
            dispatch_receiver: Some(entry),
            param_checks: Vec::new(),
        });
        ir.fn_source_order.insert(function, order);
        function
    };
    let getter = method("getFirst", 10);
    let middle = method("middle", 20);
    let setter_body = ir.add_expr(IrExpr::Return(None));
    let setter = ir.add_fun(IrFunction {
        name: "setFirst".into(),
        params: vec![Ty::String],
        ret: Ty::Unit,
        body: Some(setter_body),
        is_static: false,
        dispatch_receiver: Some(entry),
        param_checks: Vec::new(),
    });
    ir.fn_source_order.insert(setter, 10);
    let optional_ty = Ty::nullable(Ty::fun(Vec::new(), Ty::String));
    let mut subclass = IrClass::synthetic(entry);
    subclass.is_enum_entry = true;
    subclass.superclass = owner;
    subclass.fields = vec![
        IrField::new("first".into(), Ty::String),
        IrField::new("optional".into(), optional_ty),
    ];
    let mut first = IrProperty::stored_value("first".into(), Ty::String, 0);
    first.source_order = 10;
    first.getter = Some(getter);
    first.setter = Some(setter);
    first.is_var = true;
    let mut optional = IrProperty::stored_value("optional".into(), optional_ty, 1);
    optional.source_order = 30;
    subclass.properties = vec![first, optional];
    subclass.methods = vec![getter, setter, middle];
    ir.add_class(subclass);

    let run = EmitRun::default();
    let classes = emit_for_test(&ir, "EnumScheduleFixture", &run).expect("emitted enum classes");
    let bytes = &classes
        .iter()
        .find(|(name, _)| name == "ScheduledEnum$ENTRY")
        .expect("entry subclass")
        .1;
    let info = parse_class(bytes).expect("readable entry class");
    assert_eq!(
        info.fields
            .into_iter()
            .map(|field| (field.name, field.descriptor, field.signature))
            .collect::<Vec<_>>(),
        vec![
            ("first".into(), "Ljava/lang/String;".into(), None),
            (
                "optional".into(),
                "Lkotlin/jvm/functions/Function0;".into(),
                Some("Lkotlin/jvm/functions/Function0<Ljava/lang/String;>;".into()),
            ),
        ]
    );
    assert_eq!(
        info.methods
            .into_iter()
            .map(|method| (method.name, method.descriptor))
            .collect::<Vec<_>>(),
        vec![
            ("<init>".into(), "(Ljava/lang/String;I)V".into()),
            ("getFirst".into(), "()Ljava/lang/String;".into()),
            ("setFirst".into(), "(Ljava/lang/String;)V".into()),
            ("middle".into(), "()Ljava/lang/String;".into()),
            (
                "getOptional".into(),
                "()Lkotlin/jvm/functions/Function0;".into()
            ),
        ]
    );
}
