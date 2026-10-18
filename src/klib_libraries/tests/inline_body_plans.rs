//! Inline body plans decoded from serialized KLIB bodies: the `let` and `also` shapes publish an
//! `InvokeLambda` plan, and any declaration or body outside those shapes publishes none.

use super::super::inline_body_plans::decode;
use super::*;
use crate::libraries::{InlineBodyPlan, InlineBodyValue};
use crate::metadata::id_signature::MetadataClass;
use crate::metadata::klib_ir::tree::{
    KlibIrAnnotation, KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrDeclarationBase,
    KlibIrExprId, KlibIrExprKind, KlibIrFunction, KlibIrMemberAccess, KlibIrParameter,
    KlibIrStatement, KlibIrType,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};

/// The public signature of `name` declared in class `owner` of `package`.
fn member_symbol(
    kind: KlibIrSymbolKind,
    package: &[&str],
    owner: &str,
    name: &str,
) -> KlibIrSymbol {
    let package = segments(package);
    let signature = package_function_signature(
        MetadataContainer {
            package: &package,
            classes: &[MetadataClass {
                name: owner,
                type_params: &[],
                expect: false,
            }],
            native_interop_library: false,
        },
        &function(name, None, Vec::new()),
    )
    .expect("the fixture member is signable");
    KlibIrSymbol {
        kind,
        signature: KlibIrSignature::Public(signature),
    }
}

fn local_symbol(kind: KlibIrSymbolKind, slot: u32) -> KlibIrSymbol {
    KlibIrSymbol {
        kind,
        signature: KlibIrSignature::FileLocal { file: 0, slot },
    }
}

/// An inline extension `fun <T> T.name(block: (T) -> R)` whose body the caller builds from the
/// receiver read, the block read and the function's own symbol.
struct Fixture {
    arena: KlibIrArena,
    receiver: KlibIrSymbol,
    block: KlibIrSymbol,
    symbol: KlibIrSymbol,
}

impl Fixture {
    fn new() -> Self {
        Self {
            arena: KlibIrArena::default(),
            receiver: local_symbol(KlibIrSymbolKind::ValueParameter, 1),
            block: local_symbol(KlibIrSymbolKind::ValueParameter, 2),
            symbol: local_symbol(KlibIrSymbolKind::Function, 3),
        }
    }

    fn read(&mut self, symbol: &KlibIrSymbol) -> KlibIrExprId {
        self.arena.push_expr(
            None,
            KlibIrExprKind::GetValue {
                symbol: symbol.clone(),
                origin: None,
            },
        )
    }

    /// `block.invoke(operands)` through `kotlin.<function_class>.invoke`.
    fn invoke(&mut self, function_class: &str, operands: &[KlibIrSymbol]) -> KlibIrExprId {
        let block = self.block.clone();
        let mut arguments = vec![Some(self.read(&block))];
        for operand in operands {
            arguments.push(Some(self.read(operand)));
        }
        self.arena.push_expr(
            None,
            KlibIrExprKind::Call {
                access: KlibIrMemberAccess {
                    symbol: member_symbol(
                        KlibIrSymbolKind::Function,
                        &["kotlin"],
                        function_class,
                        "invoke",
                    ),
                    arguments: KlibIrArguments::Flat(arguments),
                    type_arguments: Vec::new(),
                    origin: Some("INVOKE".to_owned()),
                },
                super_qualifier: None,
            },
        )
    }

    fn returning(&mut self, value: KlibIrExprId) -> KlibIrExprId {
        self.arena.push_expr(
            None,
            KlibIrExprKind::Return {
                target: self.symbol.clone(),
                value,
            },
        )
    }

    /// The function with `statements` as its body, annotated `@InlineOnly` when `inline_only`.
    fn function(&mut self, statements: Vec<KlibIrExprId>, inline_only: bool) -> KlibIrFunction {
        let ty = self.arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        let parameter = |symbol: &KlibIrSymbol, name: &str| KlibIrParameter {
            base: KlibIrDeclarationBase {
                symbol: symbol.clone(),
                origin: "DEFINED".to_owned(),
                flags: 0,
                annotations: Vec::new(),
            },
            name: name.to_owned(),
            ty,
            vararg_element_type: None,
            default_value: None,
        };
        let annotations = inline_only
            .then(|| KlibIrAnnotation {
                access: KlibIrMemberAccess {
                    symbol: member_symbol(
                        KlibIrSymbolKind::Constructor,
                        &["kotlin", "internal"],
                        "InlineOnly",
                        "<init>",
                    ),
                    arguments: KlibIrArguments::Flat(Vec::new()),
                    type_arguments: Vec::new(),
                    origin: None,
                },
                constructor_type_arguments: 0,
            })
            .into_iter()
            .collect();
        KlibIrFunction {
            base: KlibIrDeclarationBase {
                symbol: self.symbol.clone(),
                origin: "DEFINED".to_owned(),
                flags: 0,
                annotations,
            },
            name: "scope".to_owned(),
            constructor: false,
            type_parameters: Vec::new(),
            dispatch_receiver: None,
            context_parameters: Vec::new(),
            extension_receiver: Some(parameter(&self.receiver, "<this>")),
            regular_parameters: vec![parameter(&self.block, "block")],
            return_type: ty,
            overridden: Vec::new(),
            companion_extension_class: None,
            prepared_inline_file: None,
            body: Some(KlibIrBody::Block(
                statements
                    .into_iter()
                    .map(KlibIrStatement::Expression)
                    .collect(),
            )),
        }
    }
}

fn inline_declaration() -> KotlinFunction {
    let mut declaration = function(
        "scope",
        Some(class("kotlin/Any", true)),
        vec![class("kotlin/Function1", false)],
    );
    declaration.is_inline = true;
    declaration
}

#[test]
fn a_let_shaped_body_returns_its_lambda_invocation() {
    let mut fixture = Fixture::new();
    let receiver = fixture.receiver.clone();
    let invocation = fixture.invoke("Function1", &[receiver]);
    let returned = fixture.returning(invocation);
    let body = fixture.function(vec![returned], true);
    let Some(InlineBodyPlan::InvokeLambda {
        frame,
        lambda_parameter,
        arguments,
        prologue,
        cleanup,
        cause,
        recovery,
        defaults,
        result,
    }) = decode(&inline_declaration(), &fixture.arena, &body)
    else {
        panic!("a let-shaped inline body publishes an invoke plan")
    };
    assert_eq!(frame.callee.as_ref(), "scope");
    assert!(frame.inline_only);
    assert_eq!(frame.source, None);
    assert_eq!(lambda_parameter, 1);
    assert_eq!(arguments, [InlineBodyValue::Parameter(0)]);
    assert!(prologue.is_empty() && cleanup.is_empty() && defaults.is_empty());
    assert!(cause.is_none() && recovery.is_none());
    assert_eq!(result, None);
}

#[test]
fn an_also_shaped_body_returns_its_receiver_after_the_invocation() {
    let mut fixture = Fixture::new();
    let receiver = fixture.receiver.clone();
    let invocation = fixture.invoke("Function1", &[receiver.clone()]);
    let read = fixture.read(&receiver);
    let returned = fixture.returning(read);
    let body = fixture.function(vec![invocation, returned], false);
    let Some(InlineBodyPlan::InvokeLambda {
        frame,
        lambda_parameter,
        arguments,
        result,
        ..
    }) = decode(&inline_declaration(), &fixture.arena, &body)
    else {
        panic!("an also-shaped inline body publishes an invoke plan")
    };
    assert!(!frame.inline_only);
    assert_eq!(lambda_parameter, 1);
    assert_eq!(arguments, [InlineBodyValue::Parameter(0)]);
    assert_eq!(result, Some(InlineBodyValue::Parameter(0)));
}

#[test]
fn a_function_that_is_not_inline_publishes_no_plan() {
    let mut fixture = Fixture::new();
    let receiver = fixture.receiver.clone();
    let invocation = fixture.invoke("Function1", &[receiver]);
    let returned = fixture.returning(invocation);
    let body = fixture.function(vec![returned], false);
    let mut declaration = inline_declaration();
    declaration.is_inline = false;
    assert!(decode(&declaration, &fixture.arena, &body).is_none());
}

#[test]
fn an_invoke_of_another_arity_or_of_the_lambda_itself_publishes_no_plan() {
    let mut fixture = Fixture::new();
    let receiver = fixture.receiver.clone();
    let invocation = fixture.invoke("Function2", &[receiver]);
    let returned = fixture.returning(invocation);
    let body = fixture.function(vec![returned], false);
    assert!(decode(&inline_declaration(), &fixture.arena, &body).is_none());

    let mut fixture = Fixture::new();
    let block = fixture.block.clone();
    let invocation = fixture.invoke("Function1", &[block]);
    let returned = fixture.returning(invocation);
    let body = fixture.function(vec![returned], false);
    assert!(decode(&inline_declaration(), &fixture.arena, &body).is_none());
}

#[test]
fn a_body_that_does_more_than_invoke_publishes_no_plan() {
    let mut fixture = Fixture::new();
    let receiver = fixture.receiver.clone();
    let first = fixture.invoke("Function1", &[receiver.clone()]);
    let second = fixture.invoke("Function1", &[receiver]);
    let returned = fixture.returning(second);
    let body = fixture.function(vec![first, returned], false);
    assert!(decode(&inline_declaration(), &fixture.arena, &body).is_none());
}
