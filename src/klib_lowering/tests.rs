//! Lowering of KLIB bodies: hand-built trees for every decline, and the Kotlin/Native stdlib for
//! the bodies it serializes (`KRUSTY_KOTLIN_NATIVE`, as `tests/klib_semantic_e2e.rs` reads it).

use std::path::PathBuf;

use super::*;
use crate::backend::{BackendCallableFact, CheckedBackendCallables};
use crate::fir::SourceFileId;
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use crate::klib::KlibArchive;
use crate::klib_libraries::{KlibDeclarationBodies, KlibLibraries};
use crate::libraries::KlibDeclarationSignature;
use crate::metadata::id_signature::{
    callable_signature, metadata_class_signature, CallableShape, DeclarationContainer,
    KlibPublicIdSignature, MetadataClass, MetadataContainer, Placement,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrBranch, KlibIrDeclarationBase, KlibIrExprId,
    KlibIrExprKind, KlibIrFunction, KlibIrMember, KlibIrMemberAccess, KlibIrNullability,
    KlibIrParameter, KlibIrStatement, KlibIrSyntheticBody, KlibIrType, KlibIrTypeId,
};
use crate::metadata::klib_ir::{
    read_declaration_trees, KlibIrConstant, KlibIrDeclarationTree, KlibIrModuleTrees,
    KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind,
};
use crate::metadata::semantic::{parse_package_fragment_checked, KotlinFunction, KotlinPackage};
use crate::metadata::semantic::{KotlinFunctionTypeShape, KotlinType};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, Ty, Visibility};

/// The fact the backend handoff freezes for the one function of `package` named `name` with this
/// receiver and these value parameters.
fn frozen(
    libraries: &KlibLibraries,
    package: &str,
    name: &str,
    receiver: Ty,
    params: &[Ty],
) -> BackendCallableFact {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name(package)), name);
    let selected = symbols
        .callables
        .functions()
        .iter()
        .filter(|function| {
            function.semantic_receiver() == Some(receiver)
                && function.semantic_params().as_ref() == params
        })
        .collect::<Vec<_>>();
    let [function] = selected.as_slice() else {
        panic!(
            "expected one `{name}` on {receiver:?}, found {}",
            selected.len()
        );
    };
    let target = function
        .callable
        .external_identity
        .expect("a KLIB function carries its provider identity");
    let mut ir = IrFile::default();
    ir.add_expr(IrExpr::Call {
        callee: Callee::External {
            target,
            default_provider: None,
            params: function.callable.params.clone(),
            ret: function.callable.ret,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        },
        dispatch_receiver: None,
        args: Vec::new(),
    });
    CheckedBackendCallables::freeze(&ir, libraries)
        .expect("the provider answers for its identity")
        .callable(target)
        .expect("the selected call is frozen")
        .clone()
}

fn validated(ir: &IrFile) {
    assert_eq!(ir.validate_determined_types(), Ok(()));
    assert_eq!(ir.validate_semantic_contracts(), Ok(()));
    // A dependency unit declares and references no property of any source file.
    assert_eq!(
        ir.validate_complete_facts(SourceFileId::from_raw(0)),
        Ok(())
    );
}

fn type_name_of(ty: Ty) -> String {
    match ty {
        Ty::Int => "Int".to_owned(),
        Ty::Long => "Long".to_owned(),
        Ty::Float => "Float".to_owned(),
        Ty::Double => "Double".to_owned(),
        Ty::Boolean => "Boolean".to_owned(),
        Ty::Nothing => "Nothing".to_owned(),
        Ty::Unit => "Unit".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Every fact a backend reads about an expression of a lowered body, as one text: its node, the
/// logical type, a return's depth (`@0`), a stable binding read (`!`) and a `when`'s
/// exhaustiveness (`exhaustive:`) and callable scope (`scope`).
fn render(ir: &IrFile, expression: ExprId) -> String {
    let node = match ir.expr(expression) {
        IrExpr::Block { stmts, value } => {
            let statements = stmts
                .iter()
                .map(|statement| render(ir, *statement))
                .collect::<Vec<_>>()
                .join("; ");
            let value = value.map_or(String::new(), |value| format!(" => {}", render(ir, value)));
            let scope = if ir.callable_scopes.contains(&expression) {
                "scope "
            } else {
                ""
            };
            format!("{scope}{{{statements}{value}}}")
        }
        IrExpr::Return(value) => format!(
            "return@{:?} {}",
            ir.checked_return_depths.get(&expression),
            value.map_or(String::new(), |value| render(ir, value))
        ),
        IrExpr::When { branches } => {
            let branches = branches
                .iter()
                .map(|(condition, result)| {
                    format!(
                        "{} -> {}",
                        condition.map_or("else".to_owned(), |condition| render(ir, condition)),
                        render(ir, *result)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let exhaustive = ir
                .whens
                .exhaustive
                .get(&expression)
                .map_or(String::new(), |ty| {
                    format!("exhaustive:{} ", type_name_of(*ty))
                });
            format!("{exhaustive}when[{branches}]")
        }
        IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
            format!("{op:?}({}, {})", render(ir, *lhs), render(ir, *rhs))
        }
        IrExpr::GetValue(slot) => {
            let stable = match ir.binding_read_stability.get(&expression) {
                Some(crate::ir::IrBindingStability::Stable) => "!",
                Some(crate::ir::IrBindingStability::Mutable) => "~",
                None => "",
            };
            format!("v{slot}{stable}")
        }
        IrExpr::Const(constant) => format!("{constant:?}"),
        other => panic!("a lowered body holds no {other:?}"),
    };
    match ir.logical_types.get(&expression) {
        Some(ty) => format!("{node}: {}", type_name_of(*ty)),
        None => node,
    }
}

/// The rendered body checked FIR lowering produces for the one function of `source` named `name`.
fn source_body(source: &str, name: &str) -> String {
    let ir = crate::fir_lower::tests::lower_single_source_with_platform(
        source,
        "KlibBodyEquivalent",
        Box::new(crate::libraries::EmptySymbolSource),
    );
    let functions = ir
        .fn_source_names
        .iter()
        .filter(|(_, source_name)| source_name.as_str() == name)
        .map(|(function, _)| *function)
        .collect::<Vec<_>>();
    let [function] = functions.as_slice() else {
        panic!("expected one source `{name}`, found {}", functions.len());
    };
    render(&ir, ir.functions[*function as usize].body.expect("a body"))
}

fn lowered_body(unit: &DependencyBodyUnit, function: FunId) -> String {
    let ir = unit.ir();
    render(ir, ir.functions[function as usize].body.expect("a body"))
}

// --- Hand-built trees -------------------------------------------------------------------------

fn kotlin_class(name: &str) -> KotlinType {
    KotlinType::Class {
        internal: format!("kotlin/{name}"),
        args: Vec::new(),
        nullable: false,
        shape: KotlinFunctionTypeShape::default(),
    }
}

fn coerce_declaration() -> KotlinFunction {
    KotlinFunction {
        name: "coerceAtLeast".to_owned(),
        receiver: Some(kotlin_class("Int")),
        params: vec![kotlin_class("Int")],
        ret: kotlin_class("Int"),
        formals: Vec::new(),
        param_names: vec!["minimumValue".to_owned()],
        param_defaults: vec![false],
        vararg: None,
        visibility: Visibility::Public,
        modality: crate::metadata::semantic::KotlinModality::Final,
        is_inline: false,
        has_reified_type_params: false,
        is_suspend: false,
        is_operator: false,
        is_infix: false,
        is_expect: false,
        is_static: false,
        context_count: 0,
        context_kinds: Vec::new(),
        annotations: Vec::new(),
        return_value_status: Default::default(),
        contract: None,
    }
}

fn public_symbol(kind: KlibIrSymbolKind, signature: KlibPublicIdSignature) -> KlibIrSymbol {
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

fn base(symbol: KlibIrSymbol) -> KlibIrDeclarationBase {
    KlibIrDeclarationBase {
        symbol,
        origin: "DEFINED".to_owned(),
        flags: 0,
        annotations: Vec::new(),
    }
}

fn less_int() -> KlibPublicIdSignature {
    let package = ["kotlin", "internal", "ir"].map(str::to_owned);
    let int = kotlin_class("Int");
    callable_signature(
        DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        },
        &CallableShape {
            name: "less",
            contexts: Vec::new(),
            receiver: None,
            params: vec![&int, &int],
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .expect("a non-generic shape is signable")
}

/// The pieces a hand-built `Int.coerceAtLeast` body is assembled from.
struct Parts {
    int: KlibIrTypeId,
    boolean: KlibIrTypeId,
    nothing: KlibIrTypeId,
    function: KlibIrSymbol,
    receiver: KlibIrSymbol,
    value: KlibIrSymbol,
}

impl Parts {
    fn read(&self, arena: &mut KlibIrArena, symbol: &KlibIrSymbol) -> KlibIrExprId {
        arena.push_expr(
            Some(self.int),
            KlibIrExprKind::GetValue {
                symbol: symbol.clone(),
                origin: None,
            },
        )
    }

    fn call(&self, arena: &mut KlibIrArena, callee: KlibPublicIdSignature) -> KlibIrExprId {
        let this = self.read(arena, &self.receiver);
        let minimum = self.read(arena, &self.value);
        arena.push_expr(
            Some(self.boolean),
            KlibIrExprKind::Call {
                access: KlibIrMemberAccess {
                    symbol: public_symbol(KlibIrSymbolKind::Function, callee),
                    arguments: KlibIrArguments::Flat(vec![Some(this), Some(minimum)]),
                    type_arguments: Vec::new(),
                    origin: Some("LT".to_owned()),
                },
                super_qualifier: None,
            },
        )
    }

    /// `if (condition) result else this`.
    fn conditional(
        &self,
        arena: &mut KlibIrArena,
        condition: KlibIrExprId,
        result: KlibIrExprId,
    ) -> KlibIrExprId {
        let otherwise = arena.push_expr(
            Some(self.boolean),
            KlibIrExprKind::Const(KlibIrConstant::Boolean(true)),
        );
        let this = self.read(arena, &self.receiver);
        arena.push_expr(
            Some(self.int),
            KlibIrExprKind::When {
                branches: vec![
                    KlibIrBranch { condition, result },
                    KlibIrBranch {
                        condition: otherwise,
                        result: this,
                    },
                ],
                origin: Some("IF".to_owned()),
            },
        )
    }

    fn return_from(
        &self,
        arena: &mut KlibIrArena,
        target: KlibIrSymbol,
        value: KlibIrExprId,
    ) -> KlibIrStatement {
        KlibIrStatement::Expression(
            arena.push_expr(Some(self.nothing), KlibIrExprKind::Return { target, value }),
        )
    }

    /// The stdlib's body: `return if (this < minimumValue) minimumValue else this`.
    fn stdlib_body(&self, arena: &mut KlibIrArena) -> Option<KlibIrBody> {
        let less = self.call(arena, less_int());
        let minimum = self.read(arena, &self.value);
        let conditional = self.conditional(arena, less, minimum);
        Some(KlibIrBody::Block(vec![self.return_from(
            arena,
            self.function.clone(),
            conditional,
        )]))
    }
}

/// A provider publishing `kotlin.ranges.coerceAtLeast(Int)` and the frozen selection of it, with
/// the bodies of one library whose `coerceAtLeast` body `body` builds.
fn coerce_fixture(
    body: impl FnOnce(&Parts, &mut KlibIrArena) -> Option<KlibIrBody>,
) -> (BackendCallableFact, KlibDeclarationBodies) {
    let libraries = KlibLibraries::from_packages(vec![(
        vec!["kotlin".to_owned(), "ranges".to_owned()],
        KotlinPackage {
            functions: vec![coerce_declaration()],
            ..KotlinPackage::default()
        },
    )])
    .expect("the declaration is signable");
    let fact = frozen(
        &libraries,
        "kotlin/ranges",
        "coerceAtLeast",
        Ty::Int,
        &[Ty::Int],
    );
    let Some(KlibDeclarationSignature::Public(signature)) = fact.declaration_signature.clone()
    else {
        panic!("a top-level KLIB function freezes its public signature");
    };
    let mut arena = KlibIrArena::default();
    let class = |name| {
        let kotlin = ["kotlin".to_owned()];
        let signature = metadata_class_signature(
            MetadataContainer {
                package: &kotlin,
                classes: &[],
                native_interop_library: false,
            },
            MetadataClass {
                name,
                type_params: &[],
                expect: false,
            },
        );
        KlibIrType::Simple {
            classifier: public_symbol(KlibIrSymbolKind::Class, signature),
            nullability: KlibIrNullability::NotSpecified,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        }
    };
    let parts = Parts {
        int: arena.push_type(class("Int")),
        boolean: arena.push_type(class("Boolean")),
        nothing: arena.push_type(class("Nothing")),
        function: public_symbol(KlibIrSymbolKind::Function, signature),
        receiver: local_symbol(KlibIrSymbolKind::ReceiverParameter, 1),
        value: local_symbol(KlibIrSymbolKind::ValueParameter, 2),
    };
    let parameter = |symbol: &KlibIrSymbol, name: &str| KlibIrParameter {
        base: base(symbol.clone()),
        name: name.to_owned(),
        ty: parts.int,
        vararg_element_type: None,
        default_value: None,
    };
    let function = KlibIrFunction {
        base: base(parts.function.clone()),
        name: "coerceAtLeast".to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: Some(parameter(&parts.receiver, "<this>")),
        regular_parameters: vec![parameter(&parts.value, "minimumValue")],
        return_type: parts.int,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: body(&parts, &mut arena),
    };
    let function = arena.push_function(function);
    let trees = KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(function),
    }])
    .expect("one tree defines each identity once");
    let bodies = KlibDeclarationBodies::from_libraries(vec![trees]).expect("one library");
    (fact, bodies)
}

/// The complete decline message of lowering `coerceAtLeast` with the body `body` builds, and
/// proof that the unit was left as it was.
fn declined(body: impl FnOnce(&Parts, &mut KlibIrArena) -> Option<KlibIrBody>) -> String {
    let (fact, bodies) = coerce_fixture(body);
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(fact.klib_body_callable().expect("signed"), &bodies)
        .expect_err("the body declines");
    assert_eq!(
        Some(decline.declaration()),
        fact.declaration_signature.as_ref()
    );
    assert!(unit.ir().exprs.is_empty());
    assert!(unit.ir().functions.is_empty());
    assert!(unit.ir().logical_types.is_empty());
    assert!(unit.ir().binding_read_stability.is_empty());
    decline.to_string()
}

const INT_COERCE_AT_LEAST: &str = "fun Int.coerceAtLeast(minimumValue: Int): Int {\n\
     return if (this < minimumValue) minimumValue else this\n\
     }\n";

const LOWERED_INT_COERCE_AT_LEAST: &str = "scope {scope {return@Some(0) when[\
     Lt(v0: Int, v1!: Int): Boolean -> v1!: Int, else -> v0: Int]: Int: Nothing}: Nothing}";

#[test]
fn a_built_in_relation_lowers_as_the_source_comparison_does() {
    let (fact, bodies) = coerce_fixture(|parts, arena| parts.stdlib_body(arena));
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(fact.klib_body_callable().expect("signed"), &bodies)
        .expect("the stdlib body shape lowers");

    validated(unit.ir());
    assert_eq!(lowered_body(&unit, function), LOWERED_INT_COERCE_AT_LEAST);
    assert_eq!(
        source_body(INT_COERCE_AT_LEAST, "coerceAtLeast"),
        LOWERED_INT_COERCE_AT_LEAST
    );
    let declaration = &unit.ir().functions[function as usize];
    assert_eq!(declaration.name, "coerceAtLeast");
    assert_eq!(declaration.params, [Ty::Int, Ty::Int]);
    assert_eq!(declaration.ret, Ty::Int);
    assert!(declaration.is_static);
    assert_eq!(declaration.dispatch_receiver, None);
    assert!(unit.ir().extension_receiver_fns.contains(&function));
    assert_eq!(
        unit.ir().fn_params[&function].identities,
        [
            crate::ir::IrParameterIdentity::extension_receiver(),
            crate::ir::IrParameterIdentity::source("minimumValue"),
        ]
    );
}

#[test]
fn a_signature_lowers_into_one_function() {
    let (fact, bodies) = coerce_fixture(|parts, arena| parts.stdlib_body(arena));
    let mut unit = DependencyBodyUnit::default();
    let callable = fact.klib_body_callable().expect("signed");
    let first = unit.lower_function(callable, &bodies).expect("lowers");
    let expressions = unit.ir().exprs.len();
    let second = unit.lower_function(callable, &bodies).expect("lowers");
    assert_eq!(first, second);
    assert_eq!(unit.ir().functions.len(), 1);
    assert_eq!(unit.ir().exprs.len(), expressions);
}

#[test]
fn a_body_that_throws_declines_by_its_form() {
    let message = declined(|parts, arena| {
        let less = parts.call(arena, less_int());
        let this = parts.read(arena, &parts.receiver);
        let thrown = arena.push_expr(Some(parts.nothing), KlibIrExprKind::Throw(this));
        let conditional = parts.conditional(arena, less, thrown);
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            conditional,
        )]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (it uses `throw`)"
    );
}

#[test]
fn a_call_of_a_library_declaration_declines_by_its_callee() {
    let message = declined(|parts, arena| {
        let KlibIrSignature::Public(own) = parts.function.signature.clone() else {
            unreachable!("the fixture function is public");
        };
        let call = parts.call(arena, own);
        let minimum = parts.read(arena, &parts.value);
        let conditional = parts.conditional(arena, call, minimum);
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            conditional,
        )]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` \
         (it calls `kotlin.ranges.coerceAtLeast`)"
    );
}

#[test]
fn a_call_whose_symbol_is_not_a_function_declines() {
    let message = declined(|parts, arena| {
        let this = parts.read(arena, &parts.receiver);
        let minimum = parts.read(arena, &parts.value);
        let malformed = arena.push_expr(
            Some(parts.boolean),
            KlibIrExprKind::Call {
                access: KlibIrMemberAccess {
                    symbol: public_symbol(KlibIrSymbolKind::Property, less_int()),
                    arguments: KlibIrArguments::Flat(vec![Some(this), Some(minimum)]),
                    type_arguments: Vec::new(),
                    origin: Some("LT".to_owned()),
                },
                super_qualifier: None,
            },
        );
        let minimum = parts.read(arena, &parts.value);
        let conditional = parts.conditional(arena, malformed, minimum);
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            conditional,
        )]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` \
         (its serialized declaration disagrees with the selected one: \
         a call target's symbol is not a function)"
    );
}

#[test]
fn a_return_from_another_declaration_declines() {
    let message = declined(|parts, arena| {
        let this = parts.read(arena, &parts.receiver);
        let foreign = public_symbol(KlibIrSymbolKind::Function, less_int());
        Some(KlibIrBody::Block(vec![
            parts.return_from(arena, foreign, this)
        ]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (it returns from another declaration)"
    );
}

#[test]
fn an_untyped_expression_declines() {
    let message = declined(|parts, arena| {
        let this = arena.push_expr(
            None,
            KlibIrExprKind::GetValue {
                symbol: parts.receiver.clone(),
                origin: None,
            },
        );
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            this,
        )]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (its value read has no type)"
    );
}

#[test]
fn a_read_of_an_undeclared_value_declines() {
    let message = declined(|parts, arena| {
        let other = local_symbol(KlibIrSymbolKind::Variable, 9);
        let read = parts.read(arena, &other);
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            read,
        )]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (it reads a value it does not declare)"
    );
}

#[test]
fn a_returned_value_of_another_type_declines() {
    let message = declined(|parts, arena| {
        let less = parts.call(arena, less_int());
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            less,
        )]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (it converts a value implicitly)"
    );
}

#[test]
fn an_else_marker_with_a_non_boolean_type_declines() {
    let message = declined(|parts, arena| {
        let less = parts.call(arena, less_int());
        let minimum = parts.read(arena, &parts.value);
        let malformed_else = arena.push_expr(
            Some(parts.int),
            KlibIrExprKind::Const(KlibIrConstant::Boolean(true)),
        );
        let this = parts.read(arena, &parts.receiver);
        let conditional = arena.push_expr(
            Some(parts.int),
            KlibIrExprKind::When {
                branches: vec![
                    KlibIrBranch {
                        condition: less,
                        result: minimum,
                    },
                    KlibIrBranch {
                        condition: malformed_else,
                        result: this,
                    },
                ],
                origin: Some("IF".to_owned()),
            },
        );
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            conditional,
        )]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (it converts a value implicitly)"
    );
}

#[test]
fn a_body_without_a_final_return_declines() {
    let message = declined(|parts, arena| {
        let this = parts.read(arena, &parts.receiver);
        Some(KlibIrBody::Block(vec![KlibIrStatement::Expression(this)]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (it ends without a `return`)"
    );
}

#[test]
fn a_declaration_without_a_body_declines() {
    assert_eq!(
        declined(|_, _| None),
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (its library serializes none)"
    );
    assert_eq!(
        declined(|_, _| Some(KlibIrBody::Synthetic(KlibIrSyntheticBody::EnumValues))),
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (the backend generates it)"
    );
}

#[test]
fn a_signature_no_library_serializes_declines() {
    let (fact, _) = coerce_fixture(|parts, arena| parts.stdlib_body(arena));
    let empty = KlibDeclarationBodies::from_libraries(Vec::new()).expect("no libraries");
    let decline = DependencyBodyUnit::default()
        .lower_function(fact.klib_body_callable().expect("signed"), &empty)
        .expect_err("nothing to join");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (no library serializes its declaration)"
    );
}

#[test]
fn a_signature_two_libraries_define_rejects_the_set() {
    let (fact, _) = coerce_fixture(|parts, arena| parts.stdlib_body(arena));
    let tree = || {
        let mut arena = KlibIrArena::default();
        let int = arena.push_type(KlibIrType::Dynamic {
            annotations: Vec::new(),
        });
        let Some(KlibDeclarationSignature::Public(signature)) = fact.declaration_signature.clone()
        else {
            unreachable!("the fixture function is public");
        };
        let function = arena.push_function(KlibIrFunction {
            base: base(public_symbol(KlibIrSymbolKind::Function, signature)),
            name: "coerceAtLeast".to_owned(),
            constructor: false,
            type_parameters: Vec::new(),
            dispatch_receiver: None,
            context_parameters: Vec::new(),
            extension_receiver: None,
            regular_parameters: Vec::new(),
            return_type: int,
            overridden: Vec::new(),
            companion_extension_class: None,
            prepared_inline_file: None,
            body: None,
        });
        KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Function(function),
        }])
        .expect("one tree")
    };
    let error = KlibDeclarationBodies::from_libraries(vec![tree(), tree()])
        .err()
        .expect("one identity, two bodies");
    let Some(KlibDeclarationSignature::Public(signature)) = fact.declaration_signature else {
        unreachable!("the fixture function is public");
    };
    assert_eq!(error.signature(), &KlibIrSignature::Public(signature));
    assert_eq!(error.libraries(), [0, 1]);
}

// --- The Kotlin/Native stdlib -----------------------------------------------------------------

fn distribution_root() -> Option<PathBuf> {
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if root.is_none() && std::env::var_os("KRUSTY_REQUIRE_KLIB").is_some() {
        panic!("KRUSTY_REQUIRE_KLIB is set but KRUSTY_KOTLIN_NATIVE is not");
    }
    root
}

/// The common stdlib KLIB's declarations and their bodies.
fn stdlib(root: &std::path::Path) -> (KlibLibraries, KlibDeclarationBodies) {
    let path = root.join("klib/common/stdlib");
    let archive =
        KlibArchive::open(&path).unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    let packages = archive
        .package_fragments()
        .into_iter()
        .map(|fragment| {
            let bytes = archive
                .read(&fragment.entry)
                .unwrap_or_else(|error| panic!("read {}: {error}", fragment.entry));
            let package = parse_package_fragment_checked(&bytes)
                .unwrap_or_else(|error| panic!("decode {}: {error:?}", fragment.entry));
            let segments = if fragment.package_fqname.is_empty() {
                Vec::new()
            } else {
                fragment
                    .package_fqname
                    .split('.')
                    .map(str::to_owned)
                    .collect()
            };
            (segments, package)
        })
        .collect();
    let libraries = KlibLibraries::from_packages(packages).expect("the stdlib is signable");
    let trees = read_declaration_trees(&archive).expect("the stdlib IR decodes");
    let bodies = KlibDeclarationBodies::from_libraries(vec![trees]).expect("one library");
    (libraries, bodies)
}

fn lowered_stdlib_body(
    libraries: &KlibLibraries,
    bodies: &KlibDeclarationBodies,
    unit: &mut DependencyBodyUnit,
    name: &str,
    receiver: Ty,
) -> String {
    let fact = frozen(libraries, "kotlin/ranges", name, receiver, &[receiver]);
    let function = unit
        .lower_function(fact.klib_body_callable().expect("signed"), bodies)
        .unwrap_or_else(|decline| panic!("{name} on {receiver:?}: {decline}"));
    lowered_body(unit, function)
}

#[test]
fn stdlib_coerce_bodies_lower_as_their_source_does() {
    let Some(root) = distribution_root() else {
        return;
    };
    let (libraries, bodies) = stdlib(&root);
    let mut unit = DependencyBodyUnit::default();
    for (name, relation, parameter) in [
        ("coerceAtLeast", "<", "minimumValue"),
        ("coerceAtMost", ">", "maximumValue"),
    ] {
        for receiver in ["Int", "Long", "Float", "Double"] {
            let ty = match receiver {
                "Int" => Ty::Int,
                "Long" => Ty::Long,
                "Float" => Ty::Float,
                _ => Ty::Double,
            };
            let source = format!(
                "fun {receiver}.{name}({parameter}: {receiver}): {receiver} {{\n\
                 return if (this {relation} {parameter}) {parameter} else this\n}}\n"
            );
            assert_eq!(
                lowered_stdlib_body(&libraries, &bodies, &mut unit, name, ty),
                source_body(&source, name),
                "{name} on {receiver}"
            );
        }
    }
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 8);
    assert_eq!(
        lowered_stdlib_body(&libraries, &bodies, &mut unit, "coerceAtLeast", Ty::Int),
        LOWERED_INT_COERCE_AT_LEAST
    );
    assert_eq!(
        lowered_stdlib_body(&libraries, &bodies, &mut unit, "coerceAtMost", Ty::Double),
        "scope {scope {return@Some(0) when[Gt(v0: Double, v1!: Double): Boolean -> v1!: Double, \
         else -> v0: Double]: Double: Nothing}: Nothing}"
    );
    assert_eq!(unit.ir().functions.len(), 8);
}

#[test]
fn stdlib_coerce_in_declines_by_its_throw() {
    let Some(root) = distribution_root() else {
        return;
    };
    let (libraries, bodies) = stdlib(&root);
    let fact = frozen(
        &libraries,
        "kotlin/ranges",
        "coerceIn",
        Ty::Int,
        &[Ty::Int, Ty::Int],
    );
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(fact.klib_body_callable().expect("signed"), &bodies)
        .expect_err("coerceIn throws on an empty range");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `kotlin.ranges.coerceIn` (it uses `throw`)"
    );
    assert!(unit.ir().exprs.is_empty());
}
