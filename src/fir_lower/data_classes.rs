//! Backend-neutral synthesis of Kotlin data-class declarations.
//!
//! This consumes stable declaration/property identities and common-IR storage selected earlier in
//! the FIR sink. It never reads source syntax or performs member lookup.

use crate::fir::{DeclarationFlags, DeclarationId, DeclarationKind, ResolvedModuleIndex};
use crate::ir::{ClassId, ExprId, FnParamInfo, IrBinOp, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

use super::FirFileLoweringFailure;

pub(super) fn finalize_data_classes(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(raw as u32);
        let Some(anchor) = index.declaration_anchor(declaration) else {
            continue;
        };
        let Some(header) = index.declaration_header(declaration) else {
            continue;
        };
        if anchor.kind != DeclarationKind::Classifier || !header.flags.has(DeclarationFlags::DATA) {
            continue;
        }
        let Some(class) = ir.checked_classifier_classes.get(&declaration).copied() else {
            continue;
        };
        let fields = data_property_fields(index, declaration, class, ir)?;
        if !header.flags.has(DeclarationFlags::SINGLETON) {
            synthesize_components_and_copy(index, declaration, class, &fields, ir)?;
        }
        synthesize_to_string(index, declaration, class, &fields, ir)?;
        synthesize_hash_code(index, declaration, class, &fields, ir)?;
        synthesize_equals(index, declaration, class, &fields, ir)?;
    }
    Ok(())
}

fn data_property_fields(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    class: ClassId,
    ir: &IrFile,
) -> Result<Vec<DataField>, FirFileLoweringFailure> {
    let mut properties = ir
        .checked_properties
        .values()
        .filter(|property| {
            property.class == Some(class)
                && property.flags.has(DeclarationFlags::PROPERTY_PARAMETER)
        })
        .filter_map(|property| {
            let anchor = index.declaration_anchor(property.declaration)?;
            let field_index = ir.classes[class as usize]
                .properties
                .iter()
                .find(|candidate| candidate.name == property.name)?
                .backing_field?;
            let field = &ir.classes[class as usize].fields[field_index as usize];
            Some((
                anchor.sibling,
                DataField {
                    index: field_index,
                    name: property.name.clone(),
                    ty: field.ty,
                    generic: field.type_param.is_some(),
                },
            ))
        })
        .collect::<Vec<_>>();
    let expected = ir
        .checked_properties
        .values()
        .filter(|property| {
            property.class == Some(class)
                && property.flags.has(DeclarationFlags::PROPERTY_PARAMETER)
        })
        .count();
    if properties.len() != expected {
        return Err(FirFileLoweringFailure::MissingClassifier(declaration));
    }
    properties.sort_by_key(|(ordinal, _)| *ordinal);
    Ok(properties.into_iter().map(|(_, field)| field).collect())
}

#[derive(Clone)]
struct DataField {
    index: u32,
    name: String,
    ty: Ty,
    generic: bool,
}

fn generated_method(
    index: &ResolvedModuleIndex,
    owner: DeclarationId,
    name: &str,
    ir: &IrFile,
) -> Option<u32> {
    (0..index.declaration_count()).find_map(|raw| {
        let declaration = DeclarationId::from_raw(raw as u32);
        let header = index.declaration_header(declaration)?;
        let anchor = index.declaration_anchor(declaration)?;
        (anchor.owner == Some(owner)
            && header.kind == DeclarationKind::Function
            && header.flags.has(DeclarationFlags::COMPILER_GENERATED)
            && index
                .callable_for_declaration(declaration)
                .and_then(|callable| index.callable_name(callable.id))
                == Some(name))
        .then(|| {
            index
                .callable_for_declaration(declaration)
                .and_then(|callable| ir.checked_callable_functions.get(&callable.id).copied())
        })
        .flatten()
    })
}

fn synthesize_components_and_copy(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    class: ClassId,
    fields: &[DataField],
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    for (ordinal, field) in fields.iter().enumerate() {
        let name = format!("component{}", ordinal + 1);
        let function = generated_method(index, declaration, &name, ir)
            .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
        let this = ir.add_expr(IrExpr::GetValue(0));
        let value = ir.add_expr(IrExpr::GetField {
            receiver: this,
            class,
            index: field.index,
        });
        let returned = ir.add_expr(IrExpr::Return(Some(value)));
        ir.functions[function as usize].body = Some(ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        }));
    }

    let copy = generated_method(index, declaration, "copy", ir)
        .ok_or(FirFileLoweringFailure::MissingCallable(declaration))?;
    let capture_count = fields.first().map_or(0, |field| field.index);
    let mut args = Vec::with_capacity(capture_count as usize + fields.len());
    for field in 0..capture_count {
        let this = ir.add_expr(IrExpr::GetValue(0));
        args.push(ir.add_expr(IrExpr::GetField {
            receiver: this,
            class,
            index: field,
        }));
    }
    args.extend((0..fields.len()).map(|ordinal| ir.add_expr(IrExpr::GetValue(ordinal as u32 + 1))));
    let constructed = ir.add_expr(IrExpr::New {
        internal: ir.classes[class as usize].fq_name_id(),
        args,
        ctor_params: None,
        ctor_desc: None,
        external_target: None,
    });
    let returned = ir.add_expr(IrExpr::Return(Some(constructed)));
    ir.functions[copy as usize].body = Some(ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    }));
    let defaults = fields
        .iter()
        .map(|field| {
            let this = ir.add_expr(IrExpr::GetValue(0));
            Some(ir.add_expr(IrExpr::GetField {
                receiver: this,
                class,
                index: field.index,
            }))
        })
        .collect();
    ir.fn_params.insert(
        copy,
        FnParamInfo::defaults(
            fields.iter().map(|field| field.name.clone()).collect(),
            defaults,
        ),
    );
    if !ir.private_methods.contains(&copy) {
        ir.functions[copy as usize].param_checks = fields
            .iter()
            .map(|field| {
                (field.ty.is_reference() && !field.ty.is_nullable()).then(|| field.name.clone())
            })
            .collect();
    }
    Ok(())
}

fn intrinsic(
    ir: &mut IrFile,
    operation: crate::ir::IrIntrinsic,
    args: Vec<ExprId>,
    ret: Ty,
) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: crate::ir::Callee::Intrinsic { operation, ret },
        dispatch_receiver: None,
        args,
    })
}

fn synthesize_to_string(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    class: ClassId,
    fields: &[DataField],
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    let function = match generated_method(index, declaration, "toString", ir) {
        Some(function) => function,
        None if has_method(ir, class, "toString", 0) => return Ok(()),
        None => return Err(FirFileLoweringFailure::MissingCallable(declaration)),
    };
    let internal = ir.classes[class as usize].fq_name();
    let simple = internal.rsplit(['/', '$']).next().unwrap_or(&internal);
    let singleton = ir.classes[class as usize].is_singleton();
    if singleton {
        let value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
            simple.to_owned().into(),
        )));
        let returned = ir.add_expr(IrExpr::Return(Some(value)));
        ir.functions[function as usize].body = Some(ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        }));
        ir.open_methods.insert(function);
        return Ok(());
    }
    let mut parts = Vec::new();
    let mut prefix = format!("{simple}(");
    for (ordinal, field) in fields.iter().enumerate() {
        if ordinal == 0 {
            prefix.push_str(&field.name);
            prefix.push('=');
            parts.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
                prefix.clone().into(),
            ))));
        } else {
            parts.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
                format!(", {}=", field.name).into(),
            ))));
        }
        let this = ir.add_expr(IrExpr::GetValue(0));
        let mut value = ir.add_expr(IrExpr::GetField {
            receiver: this,
            class,
            index: field.index,
        });
        if field.ty.is_array() {
            value = intrinsic(
                ir,
                crate::ir::IrIntrinsic::DataClassArrayToString { ty: field.ty },
                vec![value],
                Ty::String,
            );
        }
        parts.push(value);
    }
    if fields.is_empty() {
        parts.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(prefix.into()))));
    }
    parts.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(")".into()))));
    let value = ir.add_expr(IrExpr::StringConcat(parts));
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    ir.functions[function as usize].body = Some(ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    }));
    ir.open_methods.insert(function);
    Ok(())
}

fn synthesize_hash_code(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    class: ClassId,
    fields: &[DataField],
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    let function = match generated_method(index, declaration, "hashCode", ir) {
        Some(function) => function,
        None if has_method(ir, class, "hashCode", 0) => return Ok(()),
        None => return Err(FirFileLoweringFailure::MissingCallable(declaration)),
    };
    let hashes = fields
        .iter()
        .map(|field| field_hash(class, field, ir))
        .collect::<Vec<_>>();
    let mut statements = Vec::new();
    let value = match hashes.as_slice() {
        [] => ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0))),
        [hash] => *hash,
        [first, rest @ ..] => {
            const RESULT: u32 = 1;
            statements.push(ir.add_expr(IrExpr::Variable {
                index: RESULT,
                ty: Ty::Int,
                init: Some(*first),
                named: false,
            }));
            for hash in rest {
                let previous = ir.add_expr(IrExpr::GetValue(RESULT));
                let factor = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(31)));
                let multiplied = ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Mul,
                    lhs: previous,
                    rhs: factor,
                });
                let value = ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Add,
                    lhs: multiplied,
                    rhs: *hash,
                });
                statements.push(ir.add_expr(IrExpr::SetValue { var: RESULT, value }));
            }
            ir.add_expr(IrExpr::GetValue(RESULT))
        }
    };
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    statements.push(returned);
    ir.functions[function as usize].body = Some(ir.add_expr(IrExpr::Block {
        stmts: statements,
        value: None,
    }));
    ir.open_methods.insert(function);
    Ok(())
}

fn field_hash(class: ClassId, field: &DataField, ir: &mut IrFile) -> ExprId {
    let read = |ir: &mut IrFile| {
        let this = ir.add_expr(IrExpr::GetValue(0));
        ir.add_expr(IrExpr::GetField {
            receiver: this,
            class,
            index: field.index,
        })
    };
    let value = read(ir);
    if !field.ty.is_nullable() && !field.generic {
        return intrinsic(
            ir,
            crate::ir::IrIntrinsic::DataClassFieldHash { ty: field.ty },
            vec![value],
            Ty::Int,
        );
    }
    let null = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
    let is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::RefEq,
        lhs: value,
        rhs: null,
    });
    let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
    let non_null = read(ir);
    let hash = intrinsic(
        ir,
        crate::ir::IrIntrinsic::DataClassFieldHash { ty: field.ty },
        vec![non_null],
        Ty::Int,
    );
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(is_null), zero), (None, hash)],
    })
}

fn synthesize_equals(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    class: ClassId,
    fields: &[DataField],
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    let function = match generated_method(index, declaration, "equals", ir) {
        Some(function) => function,
        None if has_method(ir, class, "equals", 1) => return Ok(()),
        None => return Err(FirFileLoweringFailure::MissingCallable(declaration)),
    };
    let classifier = ir.classes[class as usize].fq_name_id();
    let mut statements = Vec::new();

    let this_value = ir.add_expr(IrExpr::GetValue(0));
    let other_value = ir.add_expr(IrExpr::GetValue(1));
    let identical = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::RefEq,
        lhs: this_value,
        rhs: other_value,
    });
    statements.push(return_guard(ir, identical, true));

    let other_value = ir.add_expr(IrExpr::GetValue(1));
    let wrong_type = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::NotInstanceOf,
        arg: other_value,
        type_operand: Ty::obj_name(classifier),
    });
    statements.push(return_guard(ir, wrong_type, false));

    const OTHER: u32 = 2;
    let other_value = ir.add_expr(IrExpr::GetValue(1));
    let cast = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: other_value,
        type_operand: Ty::obj_name(classifier),
    });
    statements.push(ir.add_expr(IrExpr::Variable {
        index: OTHER,
        ty: Ty::obj_name(classifier),
        init: Some(cast),
        named: false,
    }));

    for field in fields {
        let this_value = ir.add_expr(IrExpr::GetValue(0));
        let left = ir.add_expr(IrExpr::GetField {
            receiver: this_value,
            class,
            index: field.index,
        });
        let other_value = ir.add_expr(IrExpr::GetValue(OTHER));
        let right = ir.add_expr(IrExpr::GetField {
            receiver: other_value,
            class,
            index: field.index,
        });
        let equal = if matches!(
            field.ty,
            Ty::Int | Ty::Short | Ty::Byte | Ty::Char | Ty::Boolean | Ty::Long
        ) {
            ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: left,
                rhs: right,
            })
        } else {
            intrinsic(
                ir,
                crate::ir::IrIntrinsic::DataClassFieldEquals { ty: field.ty },
                vec![left, right],
                Ty::Boolean,
            )
        };
        let false_value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(false)));
        let different = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: equal,
            rhs: false_value,
        });
        statements.push(return_guard(ir, different, false));
    }

    let result = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(true)));
    statements.push(ir.add_expr(IrExpr::Return(Some(result))));
    ir.functions[function as usize].body = Some(ir.add_expr(IrExpr::Block {
        stmts: statements,
        value: None,
    }));
    ir.open_methods.insert(function);
    Ok(())
}

fn return_guard(ir: &mut IrFile, condition: u32, value: bool) -> u32 {
    let value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(value)));
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    let branch = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(condition), branch)],
    })
}

fn has_method(ir: &IrFile, class: ClassId, name: &str, parameter_count: usize) -> bool {
    ir.classes[class as usize].methods.iter().any(|function| {
        let function = &ir.functions[*function as usize];
        function.name == name && function.params.len() == parameter_count
    })
}
