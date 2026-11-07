use super::*;
use crate::klib_lowering::function_lowering::{raw_function_header_with_class, HeaderPurpose};
use crate::klib_lowering::tests::{base, local_symbol, public_symbol};
use crate::metadata::klib_ir::tree::{
    KlibIrFunction, KlibIrNullability, KlibIrParameter, KlibIrType, KlibIrTypeArgument,
    KlibIrTypeId, KlibIrVariance,
};
use crate::metadata::klib_ir::KlibIrSymbolKind;

struct NoCalls;
impl CallLinker for NoCalls {
    fn requires_declaration(&self, _: &KlibPublicIdSignature) -> Lowered<bool> {
        panic!("no call")
    }
    fn link(&mut self, _: &mut IrFile, _: &KlibPublicIdSignature) -> Lowered<LinkedCallee> {
        panic!("no call")
    }
    fn link_constructor(
        &mut self,
        _: &mut IrFile,
        _: &KlibPublicIdSignature,
    ) -> Lowered<LinkedConstructor> {
        panic!("no construction")
    }
    fn link_accessor(
        &mut self,
        _: &mut IrFile,
        _: &KlibAccessorIdSignature,
    ) -> Lowered<LinkedAccessor> {
        panic!("no accessor")
    }
    fn singleton(&self, _: TypeName) -> Lowered<()> {
        panic!("no singleton")
    }
    fn applied_supertype(&self, sub: Ty, owner: TypeName) -> Option<Ty> {
        (sub.kotlin_class_internal() == Some(owner)).then_some(sub)
    }
    fn is_subtype(&self, _: &KlibTypeParameters, sub: Ty, sup: Ty) -> bool {
        sub == sup
    }
    fn same_type(&self, left: Ty, right: Ty) -> bool {
        left == right
    }
    fn constructor_bound(&self, _: &KlibTypeParameters, argument: Ty, bound: Ty) -> bool {
        argument == bound
    }
}

fn raw_type(
    arena: &mut KlibIrArena,
    classifier: KlibIrSymbol,
    arguments: Vec<KlibIrTypeArgument>,
) -> KlibIrTypeId {
    arena.push_type(KlibIrType::Simple {
        classifier,
        nullability: KlibIrNullability::NotSpecified,
        arguments,
        annotations: vec![],
        abbreviation: None,
    })
}

struct Fixture {
    arena: KlibIrArena,
    function: KlibIrFunction,
    fields: Vec<crate::metadata::klib_ir::tree::KlibIrField>,
    types: KlibTypeParameters,
    receiver_ty: Ty,
    field_ty: Ty,
    receiver: KlibIrExprId,
    wrong_receiver: KlibIrExprId,
    value: KlibIrExprId,
    field_type: KlibIrTypeId,
    int_type: KlibIrTypeId,
    unit_type: KlibIrTypeId,
    owner: TypeName,
}

impl Fixture {
    fn new(value_member: bool) -> Self {
        let mut arena = KlibIrArena::default();
        let owner = crate::types::type_name("demo/Storage");
        let formal = local_symbol(KlibIrSymbolKind::TypeParameter, 70);
        let field_ty = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let mut types = KlibTypeParameters::default();
        assert!(types.bind(&formal, field_ty));
        let field_type = raw_type(&mut arena, formal, vec![]);
        let application = vec![KlibIrTypeArgument::Type {
            variance: KlibIrVariance::Invariant,
            ty: field_type,
        }];
        let receiver_type = raw_type(
            &mut arena,
            public_symbol(
                KlibIrSymbolKind::Class,
                KlibPublicIdSignature::classifier(owner),
            ),
            application.clone(),
        );
        let other_type = raw_type(
            &mut arena,
            public_symbol(
                KlibIrSymbolKind::Class,
                KlibPublicIdSignature::classifier(crate::types::type_name("demo/Other")),
            ),
            application,
        );
        let unit_type = raw_type(
            &mut arena,
            public_symbol(
                KlibIrSymbolKind::Class,
                KlibPublicIdSignature::classifier(crate::types::type_name("kotlin/Unit")),
            ),
            vec![],
        );
        let int_type = raw_type(
            &mut arena,
            public_symbol(
                KlibIrSymbolKind::Class,
                KlibPublicIdSignature::classifier(crate::types::type_name("kotlin/Int")),
            ),
            vec![],
        );
        let receiver_parameter = KlibIrParameter {
            base: base(local_symbol(KlibIrSymbolKind::ValueParameter, 0)),
            name: "instance".into(),
            ty: receiver_type,
            vararg_element_type: None,
            default_value: None,
        };
        let value_parameter = KlibIrParameter {
            base: base(local_symbol(KlibIrSymbolKind::ValueParameter, 1)),
            name: "value".into(),
            ty: field_type,
            vararg_element_type: None,
            default_value: None,
        };
        let receiver = arena.push_expr(
            Some(receiver_type),
            KlibIrExprKind::GetValue {
                symbol: receiver_parameter.base.symbol.clone(),
                origin: None,
            },
        );
        let wrong_receiver = arena.push_expr(
            Some(other_type),
            KlibIrExprKind::GetValue {
                symbol: receiver_parameter.base.symbol.clone(),
                origin: None,
            },
        );
        let value = arena.push_expr(
            Some(field_type),
            KlibIrExprKind::GetValue {
                symbol: value_parameter.base.symbol.clone(),
                origin: None,
            },
        );
        let mut function_base = base(local_symbol(KlibIrSymbolKind::Function, 2));
        // The existing private-value-member header authenticates this exact visibility flag.
        function_base.flags = 1 << 1;
        let function = KlibIrFunction {
            base: function_base,
            name: "body".into(),
            constructor: false,
            type_parameters: vec![],
            dispatch_receiver: value_member.then_some(receiver_parameter.clone()),
            context_parameters: vec![],
            extension_receiver: None,
            regular_parameters: if value_member {
                vec![value_parameter]
            } else {
                vec![receiver_parameter, value_parameter]
            },
            return_type: field_type,
            overridden: vec![],
            companion_extension_class: None,
            prepared_inline_file: None,
            body: None,
        };
        let fields = (0..2)
            .map(|index| crate::metadata::klib_ir::tree::KlibIrField {
                base: base(local_symbol(KlibIrSymbolKind::Field, 10 + index)),
                name: format!("slot{index}"),
                ty: field_type,
                initializer: None,
            })
            .collect();
        Self {
            arena,
            function,
            fields,
            types,
            receiver_ty: Ty::obj_args_name(owner, &[field_ty]),
            field_ty,
            receiver,
            wrong_receiver,
            value,
            field_type,
            int_type,
            unit_type,
            owner,
        }
    }

    fn read(
        &mut self,
        slot: usize,
        receiver: Option<KlibIrExprId>,
        ty: KlibIrTypeId,
    ) -> KlibIrExprId {
        self.arena.push_expr(
            Some(ty),
            KlibIrExprKind::GetField {
                symbol: self.fields[slot].base.symbol.clone(),
                super_qualifier: None,
                receiver,
                origin: None,
            },
        )
    }

    fn write(
        &mut self,
        slot: usize,
        receiver: Option<KlibIrExprId>,
        ty: KlibIrTypeId,
    ) -> KlibIrExprId {
        self.arena.push_expr(
            Some(ty),
            KlibIrExprKind::SetField {
                symbol: self.fields[slot].base.symbol.clone(),
                super_qualifier: None,
                receiver,
                value: self.value,
                origin: None,
            },
        )
    }

    fn closure(&mut self, expression: KlibIrExprId, result_type: KlibIrTypeId) -> KlibIrExprId {
        let mut function = self.function.clone();
        function.base = base(local_symbol(KlibIrSymbolKind::Function, 80));
        function.dispatch_receiver = None;
        function.context_parameters.clear();
        function.extension_receiver = None;
        function.regular_parameters.clear();
        function.return_type = result_type;
        let nothing = raw_type(
            &mut self.arena,
            public_symbol(
                KlibIrSymbolKind::Class,
                KlibPublicIdSignature::classifier(crate::types::type_name("kotlin/Nothing")),
            ),
            vec![],
        );
        let returned = self.arena.push_expr(
            Some(nothing),
            KlibIrExprKind::Return {
                target: function.base.symbol.clone(),
                value: expression,
            },
        );
        function.body = Some(crate::metadata::klib_ir::tree::KlibIrBody::Block(vec![
            KlibIrStatement::Expression(returned),
        ]));
        let function = self.arena.push_function(function);
        let signature = raw_type(
            &mut self.arena,
            public_symbol(
                KlibIrSymbolKind::Class,
                KlibPublicIdSignature::classifier(crate::types::type_name("kotlin/Function0")),
            ),
            vec![KlibIrTypeArgument::Type {
                variance: KlibIrVariance::Out,
                ty: result_type,
            }],
        );
        self.arena.push_expr(
            Some(signature),
            KlibIrExprKind::FunctionExpression {
                function,
                origin: Some("LAMBDA".into()),
            },
        )
    }

    fn lower(&self, expression: KlibIrExprId, access: Access) -> (IrFile, Lowered<ExprId>) {
        self.lower_with(expression, access, false, false)
    }

    fn lower_with(
        &self,
        expression: KlibIrExprId,
        access: Access,
        no_declared_receiver: bool,
        implicit: bool,
    ) -> (IrFile, Lowered<ExprId>) {
        let purpose = if matches!(access, Access::ValueReadOnly) {
            HeaderPurpose::PrivateValueMember(self.owner)
        } else {
            HeaderPurpose::Body(None)
        };
        let header = raw_function_header_with_class(
            None,
            purpose,
            &self.arena,
            &self.function,
            self.types.clone(),
        )
        .expect("decoded generic frame header");
        let mut ir = IrFile::default();
        let mut class = crate::ir::IrClass::synthetic(self.owner);
        class.type_params = vec!["T".into()];
        class.fields = (0..2)
            .map(|index| {
                crate::ir::IrField::new(format!("slot{index}"), self.field_ty)
                    .with_is_final(index == 0)
            })
            .collect();
        if matches!(access, Access::ValueReadOnly) {
            class.is_value = true;
            class.fields.truncate(1);
        }
        let class = ir.add_class(class);
        let function = crate::klib_lowering::function_lowering::declare_function(&mut ir, &header);
        let builtins = IrBuiltinOperators::new();
        let mut linker = NoCalls;
        let mut lowering = BodyLowering::new(
            &self.arena,
            LoweredFunction {
                symbol: &self.function.base.symbol,
                function,
            },
            &header,
            &builtins,
            &mut ir,
            &mut linker,
        );
        if matches!(access, Access::ValueReadOnly) {
            lowering = lowering.reading_value_through(class, &self.fields[0]);
            if no_declared_receiver {
                lowering.instance_storage.as_mut().unwrap().receiver = None;
            }
        } else {
            lowering.instance_storage = Some(InstanceStorageFrame {
                class,
                receiver: Some(self.receiver_ty),
                access,
                fields: self
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| Field {
                        symbol: &field.base.symbol,
                        index: index as u32,
                        ty: self.field_ty,
                    })
                    .collect(),
            });
        }
        let result = if implicit {
            lowering.expression(self.receiver).and_then(|receiver| {
                lowering.initializer_store(&self.fields[1].base.symbol, receiver, self.value)
            })
        } else {
            lowering.expression(expression)
        };
        (ir, result)
    }
}

#[test]
fn generic_multifield_read_uses_the_second_exact_field_and_applied_receiver() {
    let mut fixture = Fixture::new(false);
    let read = fixture.read(1, Some(fixture.receiver), fixture.field_type);
    let (ir, result) = fixture.lower(read, Access::Initializer);
    let result = result.expect("exact second field");
    let IrExpr::GetField {
        receiver,
        class,
        index,
    } = *ir.expr(result)
    else {
        panic!("field read")
    };
    assert_eq!((class, index), (0, 1));
    assert!(matches!(ir.expr(receiver), IrExpr::GetValue(0)));
    assert_eq!(ir.checked_type(receiver), Some(fixture.receiver_ty));
    assert_eq!(ir.checked_type(result), Some(fixture.field_ty));
}

#[test]
fn initializer_writes_final_and_mutable_fields_and_keeps_operand_order() {
    for slot in [0, 1] {
        let mut fixture = Fixture::new(false);
        let write = fixture.write(slot, Some(fixture.receiver), fixture.unit_type);
        let (ir, result) = fixture.lower(write, Access::Initializer);
        let result = result.expect("constructor write authority");
        let IrExpr::SetField {
            receiver,
            class,
            index,
            value,
        } = *ir.expr(result)
        else {
            panic!("field store")
        };
        assert_eq!((class, index), (0, slot as u32));
        assert!(receiver < value && value < result);
        assert!(matches!(ir.expr(receiver), IrExpr::GetValue(0)));
        assert!(matches!(ir.expr(value), IrExpr::GetValue(1)));
        assert_eq!(ir.checked_type(receiver), Some(fixture.receiver_ty));
        assert_eq!(ir.checked_type(value), Some(fixture.field_ty));
        assert_eq!(ir.checked_type(result), Some(Ty::Unit));
    }
}

#[test]
fn implicit_initializer_store_uses_the_same_second_field_projection() {
    let fixture = Fixture::new(false);
    let (ir, result) = fixture.lower_with(fixture.value, Access::Initializer, false, true);
    let result = result.unwrap();
    let IrExpr::SetField {
        receiver,
        class,
        index,
        value,
    } = *ir.expr(result)
    else {
        panic!("implicit field store")
    };
    assert_eq!((class, index), (0, 1));
    assert!(receiver < value && value < result);
    assert_eq!(ir.checked_type(receiver), Some(fixture.receiver_ty));
}

#[test]
fn value_member_keeps_exact_read_only_storage() {
    let mut fixture = Fixture::new(true);
    let read = fixture.read(0, Some(fixture.receiver), fixture.field_type);
    let (ir, result) = fixture.lower(read, Access::ValueReadOnly);
    let IrExpr::GetField {
        receiver,
        class,
        index,
    } = *ir.expr(result.unwrap())
    else {
        panic!("value read")
    };
    assert_eq!((class, index), (0, 0));
    assert_eq!(ir.checked_type(receiver), Some(fixture.receiver_ty));
    for slot in [0, 1] {
        let write = fixture.write(slot, Some(fixture.receiver), fixture.unit_type);
        let (_, result) = fixture.lower(write, Access::ValueReadOnly);
        assert_eq!(
            result,
            Err(KlibBodyDeclineReason::UnsupportedOperation("a field write"))
        );
    }
}

#[test]
fn malformed_owner_symbol_type_and_receiver_errors_keep_the_complete_boundary() {
    for access in [Access::Initializer, Access::ValueReadOnly] {
        let mut fixture = Fixture::new(matches!(access, Access::ValueReadOnly));
        let slot = if matches!(access, Access::Initializer) {
            1
        } else {
            0
        };
        let read = fixture.read(slot, Some(fixture.wrong_receiver), fixture.field_type);
        assert_eq!(
            fixture.lower(read, access).1,
            Err(KlibBodyDeclineReason::ImplicitConversion)
        );
        let read = fixture.read(slot, None, fixture.field_type);
        assert_eq!(
            fixture.lower(read, access).1,
            Err(mismatch("an instance field access has no receiver"))
        );
        let read = fixture.read(slot, None, fixture.int_type);
        let expected = if matches!(access, Access::Initializer) {
            "an instance field access has no receiver"
        } else {
            "a field read disagrees with its declared storage type"
        };
        assert_eq!(fixture.lower(read, access).1, Err(mismatch(expected)));
        let read = fixture.read(slot, Some(fixture.receiver), fixture.int_type);
        assert_eq!(
            fixture.lower(read, access).1,
            Err(mismatch(
                "a field read disagrees with its declared storage type"
            ))
        );
        // The same field spelling/type with a different declaration coordinate is foreign.
        let read = fixture.arena.push_expr(
            Some(fixture.field_type),
            KlibIrExprKind::GetField {
                symbol: local_symbol(KlibIrSymbolKind::Field, 999),
                super_qualifier: None,
                receiver: Some(fixture.receiver),
                origin: None,
            },
        );
        let expected = if matches!(access, Access::Initializer) {
            "an initializer accesses a field outside its exact declaring class"
        } else {
            "a value class member reads a field outside its declaring class"
        };
        assert_eq!(fixture.lower(read, access).1, Err(mismatch(expected)));
    }
}

#[test]
fn value_receiver_failure_order_and_malformed_store_type_are_unchanged() {
    let mut fixture = Fixture::new(true);
    let read = fixture.read(0, None, fixture.field_type);
    assert_eq!(
        fixture
            .lower_with(read, Access::ValueReadOnly, true, false)
            .1,
        Err(mismatch("a value class member has no receiver"))
    );
    let read = fixture.read(0, None, fixture.int_type);
    assert_eq!(
        fixture
            .lower_with(read, Access::ValueReadOnly, true, false)
            .1,
        Err(mismatch(
            "a field read disagrees with its declared storage type"
        ))
    );
    let mut fixture = Fixture::new(false);
    let write = fixture.write(1, Some(fixture.receiver), fixture.field_type);
    assert_eq!(
        fixture.lower(write, Access::Initializer).1,
        Err(mismatch("a field store is not typed Unit"))
    );
}

#[test]
fn another_declaration_owner_and_duplicate_coordinates_are_not_rebound() {
    let mut fixture = Fixture::new(false);
    let mut foreign = fixture.fields[1].base.symbol.clone();
    foreign.signature = crate::metadata::klib_ir::KlibIrSignature::FileLocal { file: 1, slot: 11 };
    let read = fixture.arena.push_expr(
        Some(fixture.field_type),
        KlibIrExprKind::GetField {
            symbol: foreign,
            super_qualifier: None,
            receiver: Some(fixture.receiver),
            origin: None,
        },
    );
    let (ir, result) = fixture.lower(read, Access::Initializer);
    assert_eq!(
        result,
        Err(mismatch(
            "an initializer accesses a field outside its exact declaring class"
        ))
    );
    assert_eq!(ir.exprs.len(), 0);
    fixture.fields[1].base.symbol = fixture.fields[0].base.symbol.clone();
    let read = fixture.read(0, Some(fixture.receiver), fixture.field_type);
    let (ir, result) = fixture.lower(read, Access::Initializer);
    assert_eq!(
        result,
        Err(mismatch(
            "a class field has an invalid or repeated storage identity"
        ))
    );
    assert_eq!(ir.exprs.len(), 0);
}

#[test]
fn initializer_value_must_have_the_declared_field_type() {
    let mut fixture = Fixture::new(false);
    let symbol = fixture.function.regular_parameters[1].base.symbol.clone();
    fixture.value = fixture.arena.push_expr(
        Some(fixture.int_type),
        KlibIrExprKind::GetValue {
            symbol,
            origin: None,
        },
    );
    let write = fixture.write(1, Some(fixture.receiver), fixture.unit_type);
    assert_eq!(
        fixture.lower(write, Access::Initializer).1,
        Err(KlibBodyDeclineReason::ImplicitConversion)
    );
}

#[test]
fn nested_initializer_closure_keeps_exact_storage_for_reads_and_writes() {
    for write in [false, true] {
        let mut fixture = Fixture::new(false);
        let (expression, result_type) = if write {
            (
                fixture.write(1, Some(fixture.receiver), fixture.unit_type),
                fixture.unit_type,
            )
        } else {
            (
                fixture.read(1, Some(fixture.receiver), fixture.field_type),
                fixture.field_type,
            )
        };
        let closure = fixture.closure(expression, result_type);
        let (ir, result) = fixture.lower(closure, Access::Initializer);
        let IrExpr::Lambda {
            impl_fn, captures, ..
        } = ir.expr(result.unwrap())
        else {
            panic!("lowered closure")
        };
        assert_eq!(captures.len(), if write { 2 } else { 1 });
        assert!(matches!(ir.expr(captures[0]), IrExpr::GetValue(0)));
        let body = ir.functions[*impl_fn as usize].body.unwrap();
        let operations = crate::ir::value_namespace_expressions(&ir, body)
            .into_iter()
            .filter_map(|id| match ir.expr(id) {
                IrExpr::GetField {
                    receiver,
                    class,
                    index,
                } => Some((false, *receiver, *class, *index, None, id)),
                IrExpr::SetField {
                    receiver,
                    class,
                    index,
                    value,
                } => Some((true, *receiver, *class, *index, Some(*value), id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(stored, receiver, class, index, value, operation)] = operations.as_slice() else {
            panic!("one exact field operation")
        };
        assert_eq!((*stored, *class, *index), (write, 0, 1));
        assert!(matches!(ir.expr(*receiver), IrExpr::GetValue(0)));
        assert_eq!(ir.checked_type(*receiver), Some(fixture.receiver_ty));
        assert_eq!(
            ir.checked_type(*operation),
            Some(if write { Ty::Unit } else { fixture.field_ty })
        );
        if let Some(value) = value {
            assert!(matches!(ir.expr(*value), IrExpr::GetValue(1)));
            assert_eq!(ir.checked_type(*value), Some(fixture.field_ty));
            assert!(receiver < value && value < operation);
        }
    }
}

#[test]
fn nested_value_member_does_not_inherit_the_parent_storage_authority() {
    for write in [false, true] {
        let mut fixture = Fixture::new(true);
        let (expression, result_type) = if write {
            (
                fixture.write(0, Some(fixture.receiver), fixture.unit_type),
                fixture.unit_type,
            )
        } else {
            (
                fixture.read(0, Some(fixture.receiver), fixture.field_type),
                fixture.field_type,
            )
        };
        let closure = fixture.closure(expression, result_type);
        let (ir, result) = fixture.lower(closure, Access::ValueReadOnly);
        assert_eq!(
            result,
            Err(KlibBodyDeclineReason::UnsupportedOperation(if write {
                "a field write"
            } else {
                "a field read"
            }))
        );
        let field_operations = ir
            .exprs
            .iter()
            .filter(|expression| {
                matches!(
                    expression,
                    IrExpr::GetField { .. } | IrExpr::SetField { .. }
                )
            })
            .count();
        assert_eq!(field_operations, 0);
    }
}
