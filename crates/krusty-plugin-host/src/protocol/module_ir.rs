//! How a [`ModuleIr`] is framed between krusty and the host: each element's offsets, then its fields
//! in declaration order. The host reads krusty's module in `adapter/ModuleIr.kt` and frames the
//! module its plugins leave the same way in `adapter/ModuleOutput.kt`.

use std::io;

use super::{
    malformed, put_callable, put_list, put_str, put_type, read_callable, read_type, Payload,
};
use krusty_plugin_api::ir::{
    BodyIr, CallIr, Callee, Constant, ExpressionIr, ExpressionKind, FileIr, FunctionIr, ModuleIr,
    ParameterIr, Span, StatementIr, ValueId, VariableIr, Visibility,
};

pub(super) fn put_module(payload: &mut Vec<u8>, module: &ModuleIr) {
    put_list(payload, &module.files, put_file);
}

fn put_file(payload: &mut Vec<u8>, file: &FileIr) {
    put_str(payload, &file.path);
    put_str(payload, &file.package);
    put_str(payload, &file.text);
    put_list(payload, &file.functions, put_function);
}

fn put_function(payload: &mut Vec<u8>, function: &FunctionIr) {
    put_span(payload, function.span);
    put_str(payload, &function.name);
    payload.push(match function.visibility {
        Visibility::Public => 0,
        Visibility::Internal => 1,
        Visibility::Private => 2,
    });
    put_list(payload, &function.parameters, put_parameter);
    put_type(payload, &function.return_type);
    put_body(payload, &function.body);
}

fn put_parameter(payload: &mut Vec<u8>, parameter: &ParameterIr) {
    put_span(payload, parameter.span);
    put_str(payload, &parameter.name);
    put_type(payload, &parameter.ty);
}

fn put_body(payload: &mut Vec<u8>, body: &BodyIr) {
    put_span(payload, body.span);
    put_list(payload, &body.statements, put_statement);
}

fn put_statement(payload: &mut Vec<u8>, statement: &StatementIr) {
    match statement {
        StatementIr::Variable(variable) => {
            payload.push(0);
            put_variable(payload, variable);
        }
        StatementIr::Expression(expression) => {
            payload.push(1);
            put_expression(payload, expression);
        }
    }
}

fn put_variable(payload: &mut Vec<u8>, variable: &VariableIr) {
    put_span(payload, variable.span);
    put_str(payload, &variable.name);
    put_type(payload, &variable.ty);
    payload.push(u8::from(variable.mutable));
    put_optional_expression(payload, &variable.initializer);
}

fn put_optional_expression(payload: &mut Vec<u8>, expression: &Option<ExpressionIr>) {
    match expression {
        Some(expression) => {
            payload.push(1);
            put_expression(payload, expression);
        }
        None => payload.push(0),
    }
}

fn put_expression(payload: &mut Vec<u8>, expression: &ExpressionIr) {
    put_span(payload, expression.span);
    match &expression.kind {
        ExpressionKind::Constant(constant) => {
            payload.push(0);
            put_constant(payload, constant);
        }
        ExpressionKind::GetValue(value) => {
            payload.push(1);
            put_u32(payload, value.0);
        }
        ExpressionKind::SetValue { value, to } => {
            payload.push(2);
            put_u32(payload, value.0);
            put_expression(payload, to);
        }
        ExpressionKind::Return(value) => {
            payload.push(3);
            put_expression(payload, value);
        }
        ExpressionKind::Call(call) => {
            payload.push(4);
            match &call.callee {
                Callee::Module { file, function } => {
                    payload.push(0);
                    put_u32(payload, *file);
                    put_u32(payload, *function);
                }
                Callee::External(identity) => {
                    payload.push(1);
                    put_callable(payload, identity);
                }
            }
            put_type(payload, &call.ty);
            put_list(payload, &call.type_arguments, put_type);
            put_list(payload, &call.arguments, put_optional_expression);
        }
    }
}

fn put_constant(payload: &mut Vec<u8>, constant: &Constant) {
    match constant {
        Constant::Boolean(value) => {
            payload.push(0);
            payload.push(u8::from(*value));
        }
        Constant::Int(value) => {
            payload.push(1);
            payload.extend_from_slice(&value.to_be_bytes());
        }
        Constant::Long(value) => {
            payload.push(2);
            payload.extend_from_slice(&value.to_be_bytes());
        }
        Constant::String(value) => {
            payload.push(3);
            put_str(payload, value);
        }
        Constant::Null(ty) => {
            payload.push(4);
            put_type(payload, ty);
        }
    }
}

fn put_span(payload: &mut Vec<u8>, span: Span) {
    payload.extend_from_slice(&span.start.to_be_bytes());
    payload.extend_from_slice(&span.end.to_be_bytes());
}

pub(super) fn read_module(payload: &mut Payload) -> io::Result<ModuleIr> {
    Ok(ModuleIr {
        files: payload.list(read_file)?,
    })
}

fn read_file(payload: &mut Payload) -> io::Result<FileIr> {
    Ok(FileIr {
        path: payload.string()?,
        package: payload.string()?,
        text: payload.string()?,
        functions: payload.list(read_function)?,
    })
}

fn read_function(payload: &mut Payload) -> io::Result<FunctionIr> {
    Ok(FunctionIr {
        span: read_span(payload)?,
        name: payload.string()?,
        visibility: match payload.byte()? {
            0 => Visibility::Public,
            1 => Visibility::Internal,
            2 => Visibility::Private,
            tag => return Err(malformed(format!("unknown visibility {tag}"))),
        },
        parameters: payload.list(|payload| {
            Ok(ParameterIr {
                span: read_span(payload)?,
                name: payload.string()?,
                ty: read_type(payload)?,
            })
        })?,
        return_type: read_type(payload)?,
        body: BodyIr {
            span: read_span(payload)?,
            statements: payload.list(read_statement)?,
        },
    })
}

fn read_statement(payload: &mut Payload) -> io::Result<StatementIr> {
    Ok(match payload.byte()? {
        0 => StatementIr::Variable(VariableIr {
            span: read_span(payload)?,
            name: payload.string()?,
            ty: read_type(payload)?,
            mutable: payload.byte()? != 0,
            initializer: read_optional_expression(payload)?,
        }),
        1 => StatementIr::Expression(read_expression(payload)?),
        tag => return Err(malformed(format!("unknown statement kind {tag}"))),
    })
}

fn read_optional_expression(payload: &mut Payload) -> io::Result<Option<ExpressionIr>> {
    match payload.byte()? {
        0 => Ok(None),
        _ => read_expression(payload).map(Some),
    }
}

fn read_expression(payload: &mut Payload) -> io::Result<ExpressionIr> {
    let span = read_span(payload)?;
    let kind = match payload.byte()? {
        0 => ExpressionKind::Constant(read_constant(payload)?),
        1 => ExpressionKind::GetValue(ValueId(payload.u32()?)),
        2 => ExpressionKind::SetValue {
            value: ValueId(payload.u32()?),
            to: Box::new(read_expression(payload)?),
        },
        3 => ExpressionKind::Return(Box::new(read_expression(payload)?)),
        4 => {
            let callee = match payload.byte()? {
                0 => Callee::Module {
                    file: payload.u32()?,
                    function: payload.u32()?,
                },
                1 => Callee::External(Box::new(read_callable(payload)?)),
                tag => return Err(malformed(format!("unknown callee kind {tag}"))),
            };
            ExpressionKind::Call(Box::new(CallIr {
                callee,
                ty: read_type(payload)?,
                type_arguments: payload.list(read_type)?,
                arguments: payload.list(read_optional_expression)?,
            }))
        }
        tag => return Err(malformed(format!("unknown expression kind {tag}"))),
    };
    Ok(ExpressionIr { span, kind })
}

fn read_constant(payload: &mut Payload) -> io::Result<Constant> {
    Ok(match payload.byte()? {
        0 => Constant::Boolean(payload.byte()? != 0),
        1 => Constant::Int(payload.i32()?),
        2 => Constant::Long(payload.i64()?),
        3 => Constant::String(payload.string()?),
        4 => Constant::Null(read_type(payload)?),
        tag => return Err(malformed(format!("unknown constant kind {tag}"))),
    })
}

fn read_span(payload: &mut Payload) -> io::Result<Span> {
    Ok(Span {
        start: payload.i32()?,
        end: payload.i32()?,
    })
}

fn put_u32(payload: &mut Vec<u8>, value: u32) {
    payload.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty_plugin_api::ir::{ClassName, Classifier, TypeIdentity};

    fn int() -> TypeIdentity {
        TypeIdentity {
            classifier: Classifier::Class {
                class: ClassName {
                    package: "kotlin".into(),
                    classes: vec!["Int".into()],
                },
                arguments: Vec::new(),
            },
            nullable: false,
            annotations: Vec::new(),
        }
    }

    /// `fun f() = 7`, private.
    fn module() -> ModuleIr {
        ModuleIr {
            files: vec![FileIr {
                path: "/a.kt".into(),
                package: String::new(),
                text: "fun f() = 7".into(),
                functions: vec![FunctionIr {
                    span: Span { start: 0, end: 11 },
                    name: "f".into(),
                    visibility: Visibility::Private,
                    parameters: Vec::new(),
                    return_type: int(),
                    body: BodyIr {
                        span: Span { start: 10, end: 11 },
                        statements: vec![StatementIr::Expression(ExpressionIr {
                            span: Span { start: 11, end: 11 },
                            kind: ExpressionKind::Return(Box::new(ExpressionIr {
                                span: Span { start: 10, end: 11 },
                                kind: ExpressionKind::Constant(Constant::Int(7)),
                            })),
                        })],
                    },
                }],
            }],
        }
    }

    #[test]
    fn a_module_is_framed_file_by_file_with_each_elements_offsets_first() {
        let int = int();
        let mut payload = Vec::new();
        put_module(&mut payload, &module());
        let mut expected = vec![0, 0, 0, 1];
        put_str(&mut expected, "/a.kt");
        put_str(&mut expected, "");
        put_str(&mut expected, "fun f() = 7");
        // One function, its offsets, its name, private, no parameters.
        expected.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 11]);
        put_str(&mut expected, "f");
        expected.extend_from_slice(&[2, 0, 0, 0, 0]);
        put_type(&mut expected, &int);
        // The body's offsets and one statement: an expression returning the constant 7.
        expected.extend_from_slice(&[0, 0, 0, 10, 0, 0, 0, 11, 0, 0, 0, 1]);
        expected.extend_from_slice(&[1, 0, 0, 0, 11, 0, 0, 0, 11, 3]);
        expected.extend_from_slice(&[0, 0, 0, 10, 0, 0, 0, 11, 0, 1, 0, 0, 0, 7]);
        assert_eq!(payload, expected);
    }

    /// The host frames the module its plugins leave as krusty frames the module it sends, with
    /// kotlinc's offsets for elements a plugin made without a source position.
    #[test]
    fn a_framed_module_reads_back_with_offsets_kotlinc_leaves_undefined() {
        let mut module = module();
        let function = &mut module.files[0].functions[0];
        function.body.statements.insert(
            0,
            StatementIr::Variable(VariableIr {
                span: Span::UNDEFINED,
                name: "tmp0".into(),
                ty: int(),
                mutable: true,
                initializer: Some(ExpressionIr {
                    span: Span::SYNTHETIC,
                    kind: ExpressionKind::Constant(Constant::Long(-1 << 40)),
                }),
            }),
        );
        function
            .body
            .statements
            .push(StatementIr::Expression(ExpressionIr {
                span: Span::UNDEFINED,
                kind: ExpressionKind::Call(Box::new(CallIr {
                    callee: Callee::Module {
                        file: 0,
                        function: 0,
                    },
                    ty: int(),
                    type_arguments: vec![int()],
                    arguments: vec![
                        None,
                        Some(ExpressionIr {
                            span: Span { start: 3, end: 4 },
                            kind: ExpressionKind::Constant(Constant::Int(-7)),
                        }),
                    ],
                })),
            }));
        let mut framed = Vec::new();
        put_module(&mut framed, &module);
        let mut payload = Payload(&framed);
        assert_eq!(read_module(&mut payload).unwrap(), module);
        assert!(payload.0.is_empty());
    }
}
