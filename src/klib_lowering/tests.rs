//! Lowering of KLIB bodies: hand-built trees for every decline, and the Kotlin/Native stdlib for
//! the bodies it serializes (`KRUSTY_KOTLIN_NATIVE`, as `tests/klib_semantic_e2e.rs` reads it).

use std::path::PathBuf;

mod body_forms;
mod callee_headers;
mod control_forms;
mod demo_package;
mod inline_templates;

use super::*;
use crate::backend::{BackendCallableFact, CheckedBackendCallables};
use crate::fir::SourceFileId;
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use crate::klib_libraries::{KlibDeclarationBodies, KlibLibraries};
use crate::libraries::{
    ExternalCallableKind, FnKind, KlibBodyCompanionReceiver, KlibDeclarationSignature,
    KlibDependencyCallable,
};
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
    KlibIrConstant, KlibIrDeclarationTree, KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol,
    KlibIrSymbolKind,
};
use crate::metadata::semantic::{KotlinFunction, KotlinPackage};
use crate::metadata::semantic::{KotlinFunctionTypeShape, KotlinType};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, Ty, TypeName, Visibility};

/// The fact the backend handoff freezes for the one function of `package` named `name` with this
/// receiver and these value parameters.
pub(super) fn frozen(
    libraries: &KlibLibraries,
    package: &str,
    name: &str,
    receiver: Ty,
    params: &[Ty],
) -> BackendCallableFact {
    frozen_function(libraries, package, name, Some(receiver), params)
}

/// The fact the backend handoff freezes for the one function of `package` named `name` with this
/// receiver, if any, and these value parameters.
pub(super) fn frozen_function(
    libraries: &KlibLibraries,
    package: &str,
    name: &str,
    receiver: Option<Ty>,
    params: &[Ty],
) -> BackendCallableFact {
    frozen_in_namespace(
        libraries,
        SymbolNamespace::Package(type_name(package)),
        name,
        receiver,
        params,
    )
}

/// The fact for an associated declaration named through `classifier` without a value operand.
fn frozen_associated_function(
    libraries: &KlibLibraries,
    classifier: &str,
    name: &str,
    params: &[Ty],
) -> BackendCallableFact {
    frozen_in_namespace(
        libraries,
        SymbolNamespace::Classifier(type_name(classifier)),
        name,
        None,
        params,
    )
}

fn frozen_in_namespace(
    libraries: &KlibLibraries,
    namespace: SymbolNamespace,
    name: &str,
    receiver: Option<Ty>,
    params: &[Ty],
) -> BackendCallableFact {
    frozen_in_namespaces(libraries, &[namespace], name, receiver, params)
}

/// Freeze one exact semantic callable from the serialized namespaces in which different KLIB
/// producers may publish that declaration. The combined set must still name one provider identity:
/// this test fixture models normalized candidate collection, not an ordered lookup fallback.
fn frozen_in_namespaces(
    libraries: &KlibLibraries,
    namespaces: &[SymbolNamespace],
    name: &str,
    receiver: Option<Ty>,
    params: &[Ty],
) -> BackendCallableFact {
    let mut selected = Vec::new();
    for &namespace in namespaces {
        let symbols = libraries.symbols(namespace, name);
        for function in symbols.callables.functions().iter().filter(|function| {
            function.semantic_receiver() == receiver
                && function.semantic_params().as_ref() == params
        }) {
            let target = function
                .callable
                .external_identity
                .expect("a KLIB function carries its provider identity");
            if !selected
                .iter()
                .any(|(candidate, _, _)| *candidate == target)
            {
                selected.push((
                    target,
                    function.callable.params.clone(),
                    function.callable.ret,
                ));
            }
        }
    }
    let [(target, callable_params, ret)] = selected.as_slice() else {
        panic!(
            "expected one `{name}` on {receiver:?}, found {}",
            selected.len()
        );
    };
    let mut ir = IrFile::default();
    ir.add_expr(IrExpr::Call {
        callee: Callee::External {
            target: *target,
            default_provider: None,
            params: callable_params.clone(),
            ret: *ret,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        },
        dispatch_receiver: None,
        args: Vec::new(),
    });
    CheckedBackendCallables::freeze(&ir, libraries)
        .expect("the provider answers for its identity")
        .callable(*target)
        .expect("the selected call is frozen")
        .clone()
}

/// Freeze the ordinary floating-point `hashCode` declaration from either KLIB representation used
/// by the supported distributions. Older stdlibs publish it as a package extension; newer ones as
/// an instance member in the classifier's declaration record. These are two serialized shapes of
/// the same source-call candidate, combined before the exact provider identity is selected rather
/// than tried as ordered fallbacks.
fn frozen_floating_hash_code(
    libraries: &KlibLibraries,
    classifier: TypeName,
    receiver: Ty,
) -> BackendCallableFact {
    let mut selected = Vec::new();
    let package = libraries.symbols(SymbolNamespace::Package(type_name("kotlin")), "hashCode");
    for function in package.callables.functions().iter().filter(|function| {
        function.kind == FnKind::Extension
            && function.semantic_receiver() == Some(receiver)
            && function.semantic_params().is_empty()
    }) {
        let target = function
            .callable
            .external_identity
            .expect("a KLIB function carries its provider identity");
        if !selected
            .iter()
            .any(|(candidate, _, _)| *candidate == target)
        {
            selected.push((
                target,
                function.callable.params.clone(),
                function.callable.ret,
            ));
        }
    }
    if let Some(record) = libraries.classifier(classifier) {
        for function in record
            .declared_callables
            .get("hashCode")
            .into_iter()
            .flat_map(|callables| callables.functions().iter())
            .filter(|function| {
                function.kind == FnKind::Member && function.semantic_params().is_empty()
            })
        {
            let target = function
                .callable
                .external_identity
                .expect("a KLIB function carries its provider identity");
            if !selected
                .iter()
                .any(|(candidate, _, _)| *candidate == target)
            {
                selected.push((
                    target,
                    function.callable.params.clone(),
                    function.callable.ret,
                ));
            }
        }
    }
    let [(target, callable_params, ret)] = selected.as_slice() else {
        panic!(
            "expected one ordinary `hashCode` declaration for {classifier:?}, found {}",
            selected.len()
        );
    };
    let mut ir = IrFile::default();
    ir.add_expr(IrExpr::Call {
        callee: Callee::External {
            target: *target,
            default_provider: None,
            params: callable_params.clone(),
            ret: *ret,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        },
        dispatch_receiver: None,
        args: Vec::new(),
    });
    CheckedBackendCallables::freeze(&ir, libraries)
        .expect("the provider answers for its identity")
        .callable(*target)
        .expect("the selected call is frozen")
        .clone()
}

pub(super) fn validated(ir: &IrFile) {
    assert_eq!(ir.validate_determined_types(), Ok(()));
    assert_eq!(ir.validate_semantic_contracts(), Ok(()));
    // A dependency unit declares and references no property of any source file.
    assert_eq!(
        ir.validate_complete_facts(SourceFileId::from_raw(0)),
        Ok(())
    );
}

pub(super) fn type_name_of(ty: Ty) -> String {
    match ty {
        Ty::Int => "Int".to_owned(),
        Ty::Long => "Long".to_owned(),
        Ty::Float => "Float".to_owned(),
        Ty::Double => "Double".to_owned(),
        Ty::Boolean => "Boolean".to_owned(),
        Ty::String => "String".to_owned(),
        Ty::Nothing => "Nothing".to_owned(),
        Ty::Unit => "Unit".to_owned(),
        // Any other class without type arguments, by its qualified name (a test rendering).
        Ty::Obj(name, []) => name.render(),
        Ty::Nullable(inner) => format!("{}?", type_name_of(*inner)),
        // A type parameter by its interned name, which a test maps to its declared spelling.
        Ty::TyParam(name, _) => format!("<{name:?}>"),
        Ty::Fun(signature) => format!(
            "({}) -> {}",
            signature
                .params
                .iter()
                .map(|param| type_name_of(*param))
                .collect::<Vec<_>>()
                .join(", "),
            type_name_of(signature.ret)
        ),
        other => format!("{other:?}"),
    }
}

/// Every fact a backend reads about an expression of a lowered body, as one text: its node, the
/// logical type, a return's depth (`@0`), a stable (`!`) or mutable (`~`) binding read, a `when`'s
/// exhaustiveness (`exhaustive:`), a callable scope (`scope`) and a recorded negation
/// (`negation`), a short circuit (`and`, `or`) and a written cast (`written`). A call names its
/// callee by source name, so two units' bodies compare whatever order their functions were
/// declared in, and a loop label by its order of appearance (`@L0`), so two lowerings that name
/// their loops differently compare.
pub(super) fn render(ir: &IrFile, expression: ExprId) -> String {
    normalized_labels(&render_node(ir, expression))
}

/// `rendered` with each distinct `@label` replaced by `@L<n>`, numbered by first appearance.
fn normalized_labels(rendered: &str) -> String {
    let mut labels = Vec::<String>::new();
    let mut out = String::new();
    let mut rest = rendered;
    while let Some(at) = rest.find("@$") {
        out.push_str(&rest[..=at]);
        let label = &rest[at + 1..];
        let end = label
            .find(|character: char| {
                !(character.is_ascii_alphanumeric() || "_$".contains(character))
            })
            .unwrap_or(label.len());
        let name = &label[..end];
        let index = match labels.iter().position(|known| known == name) {
            Some(index) => index,
            None => {
                labels.push(name.to_owned());
                labels.len() - 1
            }
        };
        out.push_str(&format!("L{index}"));
        rest = &label[end..];
    }
    out.push_str(rest);
    out
}

fn render_node(ir: &IrFile, expression: ExprId) -> String {
    let render = render_node;
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
            let short_circuit = match ir.short_circuits.get(&expression) {
                Some(crate::ir::IrShortCircuitKind::And) => "and ",
                Some(crate::ir::IrShortCircuitKind::Or) => "or ",
                None => "",
            };
            format!("{short_circuit}{exhaustive}when[{branches}]")
        }
        IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
            let negation = if ir.negations.contains(&expression) {
                "negation "
            } else {
                ""
            };
            format!(
                "{negation}{op:?}({}, {})",
                render(ir, *lhs),
                render(ir, *rhs)
            )
        }
        IrExpr::Equality { op, mode, lhs, rhs } => {
            format!(
                "{op:?}.{mode:?}({}, {})",
                render(ir, *lhs),
                render(ir, *rhs)
            )
        }
        IrExpr::Call {
            callee: Callee::Local(function),
            dispatch_receiver: None,
            args,
        } => {
            let args = args
                .iter()
                .map(|argument| render(ir, *argument))
                .collect::<Vec<_>>()
                .join(", ");
            format!("call {}({args})", ir.fn_source_names[function])
        }
        IrExpr::InvokeFunction { func, args, .. } => {
            let operands = std::iter::once(func)
                .chain(args)
                .map(|operand| render(ir, *operand))
                .collect::<Vec<_>>()
                .join(", ");
            format!("invoke({operands})")
        }
        IrExpr::Variable {
            index,
            ty,
            init,
            named,
        } => format!(
            "{} v{index}: {} = {}",
            if *named { "val" } else { "temp" },
            type_name_of(*ty),
            init.map_or(String::new(), |init| render(ir, init))
        ),
        IrExpr::SetValue { var, value } => format!("v{var} = {}", render(ir, *value)),
        IrExpr::GetValue(slot) => {
            let stable = match ir.binding_read_stability.get(&expression) {
                Some(crate::ir::IrBindingStability::Stable) => "!",
                Some(crate::ir::IrBindingStability::Mutable) => "~",
                None => "",
            };
            format!("v{slot}{stable}")
        }
        IrExpr::Const(constant) => format!("{constant:?}"),
        IrExpr::While {
            cond,
            body,
            update,
            post_test,
            label,
        } => format!(
            "{}{}({}) {}{}",
            if *post_test { "do-while" } else { "while" },
            label_suffix(label.as_deref()),
            render(ir, *cond),
            render(ir, *body),
            update.map_or(String::new(), |update| format!(
                " update {}",
                render(ir, update)
            ))
        ),
        IrExpr::Break { label } => format!("break{}", label_suffix(label.as_deref())),
        IrExpr::Continue { label } => format!("continue{}", label_suffix(label.as_deref())),
        IrExpr::Throw { operand } => format!("throw {}", render(ir, *operand)),
        IrExpr::StringConcat(parts) => format!(
            "concat[{}]",
            parts
                .iter()
                .map(|part| render(ir, *part))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => format!(
            "{}{op:?}<{}>({})",
            if ir.written_casts.contains(&expression) {
                "written "
            } else {
                ""
            },
            type_name_of(*type_operand),
            render(ir, *arg)
        ),
        IrExpr::NotNullAssert { operand, check } => {
            format!("{check:?}!!({})", render(ir, *operand))
        }
        IrExpr::BottomValue {
            producer,
            completion,
        } => format!("{completion:?}({})", render(ir, *producer)),
        IrExpr::UnitInstance => "Unit".to_owned(),
        other => panic!("a lowered body holds no {other:?}"),
    };
    match ir.logical_types.get(&expression) {
        Some(ty) => format!("{node}: {}", type_name_of(*ty)),
        None => node,
    }
}

fn label_suffix(label: Option<&str>) -> String {
    label.map_or(String::new(), |label| format!("@{label}"))
}

/// The rendered body checked FIR lowering produces for the one function of `source` named `name`.
pub(super) fn source_body(source: &str, name: &str) -> String {
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

pub(super) fn lowered_body(unit: &DependencyBodyUnit, function: FunId) -> String {
    let ir = unit.ir();
    render(ir, ir.functions[function as usize].body.expect("a body"))
}

// --- Hand-built trees -------------------------------------------------------------------------

pub(super) fn kotlin_class(name: &str) -> KotlinType {
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
        inline_modifiers: vec![crate::types::InlineParameterModifier::None],
        vararg: None,
        visibility: Visibility::Public,
        modality: crate::metadata::semantic::KotlinModality::Final,
        is_inline: false,
        has_reified_type_params: false,
        is_suspend: false,
        is_operator: false,
        is_infix: false,
        is_synthesized: false,
        is_expect: false,
        is_external: false,
        is_static: false,
        context_count: 0,
        context_kinds: Vec::new(),
        annotations: Vec::new(),
        annotation_applications: Vec::new(),
        return_value_status: Default::default(),
        contract: None,
    }
}

pub(super) fn public_symbol(
    kind: KlibIrSymbolKind,
    signature: KlibPublicIdSignature,
) -> KlibIrSymbol {
    KlibIrSymbol {
        kind,
        signature: KlibIrSignature::Public(signature),
    }
}

pub(super) fn local_symbol(kind: KlibIrSymbolKind, slot: u32) -> KlibIrSymbol {
    KlibIrSymbol {
        kind,
        signature: KlibIrSignature::FileLocal { file: 0, slot },
    }
}

pub(super) fn base(symbol: KlibIrSymbol) -> KlibIrDeclarationBase {
    KlibIrDeclarationBase {
        symbol,
        origin: "DEFINED".to_owned(),
        flags: 0,
        annotations: Vec::new(),
    }
}

pub(super) fn less_int() -> KlibPublicIdSignature {
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
    coerce_fixture_with_companion_marker(None, body)
}

fn coerce_fixture_with_companion_marker(
    companion_extension: Option<&str>,
    body: impl FnOnce(&Parts, &mut KlibIrArena) -> Option<KlibIrBody>,
) -> (BackendCallableFact, KlibDeclarationBodies) {
    let libraries = KlibLibraries::from_named_packages(
        "fixture",
        vec![(
            vec!["kotlin".to_owned(), "ranges".to_owned()],
            KotlinPackage {
                functions: vec![coerce_declaration()],
                ..KotlinPackage::default()
            },
        )],
    )
    .expect("the declaration is signable");
    let mut fact = frozen(
        &libraries,
        "kotlin/ranges",
        "coerceAtLeast",
        Ty::Int,
        &[Ty::Int],
    );
    if companion_extension.is_some() {
        assert_eq!(fact.params, [Ty::Int, Ty::Int]);
        assert_eq!(
            *fact.parameter_identities,
            [
                crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
                crate::fir::ResolvedParameterIdentity::Source("minimumValue".into()),
            ]
        );
        fact.kind = ExternalCallableKind::TopLevel;
        fact.params.remove(fact.context_count);
        fact.parameter_identities = fact
            .parameter_identities
            .into_vec()
            .into_iter()
            .skip(1)
            .collect();
        fact.source_receiver = None;
        fact.klib_body_companion_receiver =
            Some(KlibBodyCompanionReceiver::new(type_name("kotlin/Int")));
    }
    let Some(KlibDeclarationSignature::Public(signature)) = fact.declaration_signature.clone()
    else {
        panic!("a top-level KLIB function freezes its public signature");
    };
    let mut arena = KlibIrArena::default();
    let class_symbol = |name| {
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
        public_symbol(KlibIrSymbolKind::Class, signature)
    };
    let class = |name| KlibIrType::Simple {
        classifier: class_symbol(name),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
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
        companion_extension_class: companion_extension.map(class_symbol),
        prepared_inline_file: None,
        body: body(&parts, &mut arena),
    };
    let function = arena.push_function(function);
    let trees = KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(function),
    }])
    .expect("one tree defines each identity once");
    let bodies = KlibDeclarationBodies::from_libraries(vec![("fixture".to_owned(), trees)])
        .expect("one library");
    (fact, bodies)
}

#[test]
fn an_unread_ordinary_extension_receiver_is_published_as_a_body_effect() {
    let (fact, bodies) = coerce_fixture(|parts, arena| {
        let value = parts.read(arena, &parts.value);
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            value,
        )]))
    });
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &bodies,
            &KlibCalleeFacts::default(),
        )
        .expect("the unread extension lowers");

    assert_eq!(
        unit.ir().singleton_extension_unread_receivers[&function],
        Ty::Int
    );
}

#[test]
fn an_unread_companion_extension_receiver_is_published_for_target_support() {
    let (fact, bodies) = coerce_fixture_with_companion_marker(Some("Int"), |parts, arena| {
        let value = parts.read(arena, &parts.value);
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            value,
        )]))
    });
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &bodies,
            &KlibCalleeFacts::default(),
        )
        .expect("the unread companion extension lowers");

    assert_eq!(
        unit.ir().singleton_extension_unread_receivers[&function],
        Ty::Int
    );
    assert_eq!(
        unit.ir().functions[function as usize].params,
        [Ty::Int, Ty::Int]
    );
    assert_eq!(
        unit.ir().fn_params[&function].identities,
        [
            crate::ir::IrParameterIdentity::extension_receiver(),
            crate::ir::IrParameterIdentity::source("minimumValue"),
        ]
    );
}

#[test]
fn a_companion_extension_body_requires_its_exact_classifier_marker() {
    let (mut fact, bodies) =
        coerce_fixture_with_companion_marker(Some("Int"), |parts, arena| parts.stdlib_body(arena));
    fact.klib_body_companion_receiver =
        Some(KlibBodyCompanionReceiver::new(type_name("kotlin/Double")));
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &bodies,
            &KlibCalleeFacts::default(),
        )
        .expect_err("a different companion classifier declines");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (its serialized declaration disagrees \
         with the selected one: the serialized companion extension classifier disagrees with \
         the selected one)"
    );
    assert!(unit.ir().functions.is_empty());
}

#[test]
fn a_header_only_companion_marker_must_match_its_extension_receiver() {
    let (fact, bodies) = coerce_fixture_with_companion_marker(Some("Double"), |parts, arena| {
        parts.stdlib_body(arena)
    });
    let (arena, function) = bodies
        .function(fact.klib_body_callable().expect("signed").identity())
        .expect("the exact declaration has a body");
    let decline = match super::function_lowering::function_header(None, arena, function) {
        Ok(_) => panic!("a transitive header cannot publish an effect from a mismatched marker"),
        Err(decline) => decline,
    };
    assert_eq!(
        decline,
        super::KlibBodyDeclineReason::SignatureMismatch(
            "the serialized companion extension classifier disagrees with its receiver".to_owned()
        )
    );
}

#[test]
fn a_read_companion_extension_receiver_is_not_published_as_unread() {
    let (fact, bodies) =
        coerce_fixture_with_companion_marker(Some("Int"), |parts, arena| parts.stdlib_body(arena));
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &bodies,
            &KlibCalleeFacts::default(),
        )
        .expect("the used companion extension still lowers");

    assert!(!unit
        .ir()
        .singleton_extension_unread_receivers
        .contains_key(&function));
}

/// The complete decline message of lowering `coerceAtLeast` with the body `body` builds, and
/// proof that the unit was left as it was.
fn declined(body: impl FnOnce(&Parts, &mut KlibIrArena) -> Option<KlibIrBody>) -> String {
    let (fact, bodies) = coerce_fixture(body);
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &bodies,
            &KlibCalleeFacts::default(),
        )
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
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &bodies,
            &KlibCalleeFacts::default(),
        )
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
    let first = unit
        .lower_function(callable, &bodies, &KlibCalleeFacts::default())
        .expect("lowers");
    let expressions = unit.ir().exprs.len();
    let second = unit
        .lower_function(callable, &bodies, &KlibCalleeFacts::default())
        .expect("lowers");
    assert_eq!(first, second);
    assert_eq!(unit.ir().functions.len(), 1);
    assert_eq!(unit.ir().exprs.len(), expressions);
}

#[test]
fn a_body_that_constructs_declines_by_its_form() {
    let message = declined(|parts, arena| {
        let less = parts.call(arena, less_int());
        // Any constructor: the form declines before its callee is read.
        let constructed = arena.push_expr(
            Some(parts.nothing),
            KlibIrExprKind::ConstructorCall {
                access: KlibIrMemberAccess {
                    symbol: public_symbol(KlibIrSymbolKind::Constructor, less_int()),
                    arguments: KlibIrArguments::Flat(Vec::new()),
                    type_arguments: Vec::new(),
                    origin: None,
                },
                constructor_type_arguments: 0,
            },
        );
        let thrown = arena.push_expr(Some(parts.nothing), KlibIrExprKind::Throw(constructed));
        let conditional = parts.conditional(arena, less, thrown);
        Some(KlibIrBody::Block(vec![parts.return_from(
            arena,
            parts.function.clone(),
            conditional,
        )]))
    });
    assert_eq!(
        message,
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (it uses a constructor call)"
    );
}

/// `kotlin.ranges.coerceAtMost(Int)` on `Int`: a library declaration no fixture selects.
fn coerce_at_most_int() -> KlibPublicIdSignature {
    let package = ["kotlin", "ranges"].map(str::to_owned);
    let int = kotlin_class("Int");
    callable_signature(
        DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        },
        &CallableShape {
            name: "coerceAtMost",
            contexts: Vec::new(),
            receiver: Some(&int),
            params: vec![&int],
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .expect("a non-generic shape is signable")
}

#[test]
fn a_call_of_a_library_declaration_declines_by_its_callee() {
    let message = declined(|parts, arena| {
        let call = parts.call(arena, coerce_at_most_int());
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
         (it calls `kotlin.ranges.coerceAtMost`, which no reachable library serializes its declaration)"
    );
}

#[test]
fn a_call_typed_apart_from_its_callee_declines() {
    let message = declined(|parts, arena| {
        let KlibIrSignature::Public(own) = parts.function.signature.clone() else {
            unreachable!("the fixture function is public");
        };
        // A recursive call is linked to the declaration being lowered, whose result is `Int`.
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
         (its serialized declaration disagrees with the selected one: \
         a call is typed apart from its callee's result)"
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
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            &empty,
            &KlibCalleeFacts::default(),
        )
        .expect_err("nothing to join");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `kotlin.ranges.coerceAtLeast` (no library serializes its declaration)"
    );
}

#[test]
fn a_signature_twice_in_one_module_rejects_the_set() {
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
    let error = KlibDeclarationBodies::from_libraries(vec![
        ("fixture".to_owned(), tree()),
        ("fixture".to_owned(), tree()),
    ])
    .err()
    .expect("one identity, two bodies");
    let Some(KlibDeclarationSignature::Public(signature)) = fact.declaration_signature else {
        unreachable!("the fixture function is public");
    };
    assert_eq!(error.signature(), Some(&KlibIrSignature::Public(signature)));
    assert_eq!(error.module(), "fixture");
    assert_eq!(error.libraries(), [0, 1]);
}

#[test]
fn two_archives_claiming_one_module_are_rejected_even_when_their_signatures_are_disjoint() {
    let (fact, _) = coerce_fixture(|parts, arena| parts.stdlib_body(arena));
    let Some(KlibDeclarationSignature::Public(signature)) = fact.declaration_signature else {
        unreachable!("the fixture function is public");
    };
    let mut arena = KlibIrArena::default();
    let result = arena.push_type(KlibIrType::Dynamic {
        annotations: Vec::new(),
    });
    let function = arena.push_function(KlibIrFunction {
        base: base(public_symbol(KlibIrSymbolKind::Function, signature)),
        name: "coerceAtLeast".to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: None,
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: Vec::new(),
        return_type: result,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: None,
    });
    let trees = KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(function),
    }])
    .expect("one tree");
    let error = KlibDeclarationBodies::from_body_libraries(vec![
        crate::klib_libraries::KlibBodyLibrary::new(
            "fixture".to_owned(),
            ["first".to_owned()],
            KlibIrModuleTrees::default(),
        ),
        crate::klib_libraries::KlibBodyLibrary::new(
            "fixture".to_owned(),
            ["second".to_owned()],
            trees,
        ),
    ])
    .err()
    .expect("one module identity cannot carry two producer graphs");
    assert_eq!(error.module(), "fixture");
    assert_eq!(error.signature(), None);
    assert_eq!(error.libraries(), [0, 1]);
}

#[test]
fn equal_internal_body_signatures_from_distinct_modules_remain_distinct() {
    let (fact, _) = coerce_fixture(|parts, arena| parts.stdlib_body(arena));
    let identity = fact
        .dependency_callable
        .clone()
        .expect("the named fixture has a module-qualified identity");
    let tree = |name: &str| {
        let mut arena = KlibIrArena::default();
        let result = arena.push_type(KlibIrType::Dynamic {
            annotations: Vec::new(),
        });
        let KlibDeclarationSignature::Public(signature) = &identity.declaration else {
            unreachable!("the fixture function is public");
        };
        let function = arena.push_function(KlibIrFunction {
            base: base(public_symbol(KlibIrSymbolKind::Function, signature.clone())),
            name: name.to_owned(),
            constructor: false,
            type_parameters: Vec::new(),
            dispatch_receiver: None,
            context_parameters: Vec::new(),
            extension_receiver: None,
            regular_parameters: Vec::new(),
            return_type: result,
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
    let bodies = KlibDeclarationBodies::from_libraries(vec![
        ("ordinary".to_owned(), tree("ordinaryBody")),
        ("friend".to_owned(), tree("friendBody")),
    ])
    .expect("equal signatures in distinct modules are legal");
    for (module, expected_name) in [("ordinary", "ordinaryBody"), ("friend", "friendBody")] {
        assert_eq!(
            bodies
                .function(&KlibDependencyCallable {
                    module: module.to_owned(),
                    declaration: identity.declaration.clone(),
                })
                .map(|(_, function)| function.name.as_str()),
            Some(expected_name)
        );
    }
}

// --- The Kotlin/Native stdlib -----------------------------------------------------------------

pub(super) fn distribution_root() -> Option<PathBuf> {
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if root.is_none() && std::env::var_os("KRUSTY_REQUIRE_KLIB").is_some() {
        panic!("KRUSTY_REQUIRE_KLIB is set but KRUSTY_KOTLIN_NATIVE is not");
    }
    root
}

/// The common stdlib KLIB's declarations and their provider-owned bodies.
pub(super) fn stdlib(root: &std::path::Path) -> KlibLibraries {
    let path = root.join("klib/common/stdlib");
    KlibLibraries::open(&[crate::klib_libraries::KlibLibraryInput::platform_stdlib(
        path,
        crate::klib_libraries::KlibStdlibSemantics::Native,
    )])
    .unwrap_or_else(|error| panic!("{error}"))
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
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            bodies,
            &KlibCalleeFacts::default(),
        )
        .unwrap_or_else(|decline| panic!("{name} on {receiver:?}: {decline}"));
    lowered_body(unit, function)
}

#[test]
fn stdlib_coerce_bodies_lower_as_their_source_does() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
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
                lowered_stdlib_body(&libraries, bodies, &mut unit, name, ty),
                source_body(&source, name),
                "{name} on {receiver}"
            );
        }
    }
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 8);
    assert_eq!(
        lowered_stdlib_body(&libraries, bodies, &mut unit, "coerceAtLeast", Ty::Int),
        LOWERED_INT_COERCE_AT_LEAST
    );
    assert_eq!(
        lowered_stdlib_body(&libraries, bodies, &mut unit, "coerceAtMost", Ty::Double),
        "scope {scope {return@Some(0) when[Gt(v0: Double, v1!: Double): Boolean -> v1!: Double, \
         else -> v0: Double]: Double: Nothing}: Nothing}"
    );
    assert_eq!(unit.ir().functions.len(), 8);
}

#[test]
fn stdlib_coerce_in_declines_by_its_exception_construction() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let fact = frozen(
        &libraries,
        "kotlin/ranges",
        "coerceIn",
        Ty::Int,
        &[Ty::Int, Ty::Int],
    );
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            bodies,
            &KlibCalleeFacts::default(),
        )
        .expect_err("coerceIn constructs the exception it throws on an empty range");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `kotlin.ranges.coerceIn` (it uses a constructor call)"
    );
    assert!(unit.ir().exprs.is_empty());
}

/// The provider handles of the authenticated intrinsics `root`'s body reaches. An intrinsic stays
/// an ordinary call of its declaration in the lowered unit; only the target emitter realizes it.
fn intrinsic_externals(
    libraries: &KlibLibraries,
    root: &KlibDependencyCallable,
) -> crate::fx_hash::HashMap<KlibDependencyCallable, super::EmittedDependencyCallable> {
    let bodies = libraries.declaration_bodies();
    let mut reached = vec![root.clone()];
    let mut pending = vec![root.clone()];
    while let Some(caller) = pending.pop() {
        for callee in bodies.direct_callees(&caller) {
            if !reached.contains(&callee) {
                reached.push(callee.clone());
                pending.push(callee);
            }
        }
    }
    reached
        .into_iter()
        .filter(|callee| libraries.intrinsic_facts().get(callee).is_some())
        .map(|callee| {
            let (external, _) =
                <KlibLibraries as crate::symbol_source::SymbolSource>::external_callable_by_dependency(
                    libraries, &callee,
                )
                .unwrap_or_else(|| panic!("the provider publishes intrinsic {callee:?}"));
            (callee, super::EmittedDependencyCallable::new(external, None, None))
        })
        .collect()
}

/// Calls in `ir` of the one reinterpretation in `externals` from `operand` to `result`.
fn reinterpret_calls(
    libraries: &KlibLibraries,
    externals: &crate::fx_hash::HashMap<KlibDependencyCallable, super::EmittedDependencyCallable>,
    ir: &IrFile,
    operand: Ty,
    result: Ty,
) -> usize {
    let targets = externals
        .iter()
        .filter(|(callee, _)| {
            libraries
                .intrinsic_facts()
                .get(callee)
                .is_some_and(|intrinsic| {
                    intrinsic.operation() == crate::libraries::CompilerIntrinsic::ReinterpretBits
                        && intrinsic.ret() == result
                })
        })
        .map(|(_, external)| external.target())
        .collect::<Vec<_>>();
    assert_eq!(
        targets.len(),
        1,
        "one reinterpretation from {operand:?} to {result:?}"
    );
    ir.exprs
        .iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::Call { callee: Callee::External { target, ret, .. }, .. }
                    if *target == targets[0] && *ret == result
            )
        })
        .count()
}

#[test]
fn native_to_bits_erases_the_producer_inline_container_and_keeps_the_intrinsic() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();
    for (operand, result) in [(Ty::Float, Ty::Int), (Ty::Double, Ty::Long)] {
        let fact = frozen(&libraries, "kotlin", "toBits", operand, &[]);
        assert!(
            bodies.is_supported_intrinsic_root(
                fact.dependency_callable
                    .as_ref()
                    .expect("toBits has an exact KLIB identity"),
                libraries.intrinsic_facts(),
            ),
            "toBits on {operand:?} directly calls its authenticated reinterpret declaration"
        );
        let callable = fact.klib_body_callable().expect("signed");
        let externals = intrinsic_externals(&libraries, callable.identity());
        let mut unit = DependencyBodyUnit::default();
        unit.lower_function_with_intrinsics_and_externals(
            callable,
            bodies,
            &KlibCalleeFacts::default(),
            libraries.intrinsic_facts(),
            &externals,
        )
        .unwrap_or_else(|decline| panic!("toBits on {operand:?}: {decline}"));
        validated(unit.ir());
        assert_eq!(unit.ir().functions.len(), 1, "toBits on {operand:?}");
        assert_eq!(
            unit.ir()
                .exprs
                .iter()
                .filter(|expression| matches!(expression, IrExpr::Variable { .. }))
                .count(),
            0,
            "producer aliases are not source locals for toBits on {operand:?}"
        );
        assert_eq!(
            reinterpret_calls(&libraries, &externals, unit.ir(), operand, result),
            2,
            "the canonical-NaN and ordinary branches each reinterpret once"
        );
        assert_eq!(
            unit.ir()
                .exprs
                .iter()
                .filter(|expression| matches!(
                    expression,
                    IrExpr::Equality {
                        mode: crate::types::EqualityMode::Ieee754,
                        ..
                    }
                ))
                .count(),
            1,
            "toBits keeps its NaN canonicalization guard"
        );
    }
}

#[test]
fn intrinsic_support_roots_exclude_ordinary_callers_of_bit_wrappers() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();

    for (classifier, receiver) in [("kotlin/Float", Ty::Float), ("kotlin/Double", Ty::Double)] {
        let fact = frozen_floating_hash_code(&libraries, type_name(classifier), receiver);
        let identity = fact
            .dependency_callable
            .as_ref()
            .expect("hashCode has an exact KLIB identity");
        let callable = fact.klib_body_callable().expect("signed");
        let has_reinterpret_reachability =
            bodies.is_supported_intrinsic_root(identity, libraries.intrinsic_facts());
        let has_reinterpret_boundary =
            DependencyBodyUnit::is_scalar_root_candidate(callable, bodies);
        if callable.is_member() {
            assert!(
                !has_reinterpret_boundary,
                "a member cannot become reinterpret support from its scalar result shape"
            );
        } else {
            assert!(
                has_reinterpret_boundary,
                "the package-extension representation has the exact scalar wrapper boundary"
            );
            assert!(
                !has_reinterpret_reachability,
                "the package-extension representation is an ordinary caller, not intrinsic support"
            );
        }
        assert!(
            !(has_reinterpret_reachability && has_reinterpret_boundary),
            "{classifier}.hashCode is an ordinary target realization, not an intrinsic support root; reachability={has_reinterpret_reachability}, boundary={has_reinterpret_boundary}"
        );
    }
}

#[test]
fn native_to_raw_bits_is_a_supported_root_and_lowers_its_authenticated_intrinsic() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let bodies = libraries.declaration_bodies();

    for (operand, result) in [(Ty::Float, Ty::Int), (Ty::Double, Ty::Long)] {
        let fact = frozen(&libraries, "kotlin", "toRawBits", operand, &[]);
        let identity = fact
            .dependency_callable
            .as_ref()
            .expect("toRawBits has an exact KLIB identity");
        assert!(
            bodies.is_supported_intrinsic_root(identity, libraries.intrinsic_facts()),
            "toRawBits on {operand:?} directly calls its authenticated reinterpret declaration"
        );
        assert!(
            DependencyBodyUnit::is_scalar_root_candidate(
                fact.klib_body_callable().expect("signed"),
                bodies,
            ),
            "toRawBits on {operand:?} has the exact scalar reinterpret boundary"
        );
        let callable = fact.klib_body_callable().expect("signed");
        let externals = intrinsic_externals(&libraries, callable.identity());
        let mut unit = DependencyBodyUnit::default();
        unit.lower_function_with_intrinsics_and_externals(
            callable,
            bodies,
            &KlibCalleeFacts::default(),
            libraries.intrinsic_facts(),
            &externals,
        )
        .unwrap_or_else(|decline| panic!("toRawBits on {operand:?}: {decline}"));
        validated(unit.ir());
        assert_eq!(unit.ir().functions.len(), 1, "toRawBits on {operand:?}");
        assert_eq!(
            reinterpret_calls(&libraries, &externals, unit.ir(), operand, result),
            1,
            "toRawBits on {operand:?} lowers exactly one reinterpret"
        );
    }
}

#[test]
fn wasm_numbers_bodies_link_their_internal_wasm_op_by_exact_intrinsic_identity() {
    let Some(path) =
        crate::toolchain::kotlin_stdlib_klib(crate::compilation_target::CompilationTarget::WasmJs)
    else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "KRUSTY_REQUIRE_KLIB is set but no Wasm/JS stdlib KLIB is provisioned"
        );
        return;
    };
    let libraries =
        KlibLibraries::open(&[crate::klib_libraries::KlibLibraryInput::platform_stdlib(
            path.clone(),
            crate::klib_libraries::KlibStdlibSemantics::Wasm,
        )])
        .unwrap_or_else(|error| panic!("{error}"));
    let bodies = libraries.declaration_bodies();

    for (name, owner, receiver, parameters, operand, result) in [
        (
            "toRawBits",
            None,
            Some(Ty::Float),
            vec![],
            Ty::Float,
            Ty::Int,
        ),
        (
            "toRawBits",
            None,
            Some(Ty::Double),
            vec![],
            Ty::Double,
            Ty::Long,
        ),
        (
            "fromBits",
            None,
            Some(Ty::obj("kotlin/Float$Companion")),
            vec![Ty::Int],
            Ty::Int,
            Ty::Float,
        ),
        (
            "fromBits",
            None,
            Some(Ty::obj("kotlin/Double$Companion")),
            vec![Ty::Long],
            Ty::Long,
            Ty::Double,
        ),
    ] {
        let fact = match owner {
            Some(owner) => frozen_associated_function(&libraries, owner, name, &parameters),
            None => frozen_function(&libraries, "kotlin", name, receiver, &parameters),
        };
        let callable = fact.klib_body_callable().expect("signed");
        assert!(
            libraries.is_platform_stdlib_dependency(callable.identity()),
            "{name} on {operand:?} reverse-joins through its exact placement"
        );
        let (arena, selected) = bodies
            .function(callable.identity())
            .expect("the exact selected declaration has a body");
        let body_calls = arena.call_symbols_in_body(selected).collect::<Vec<_>>();
        let authenticated_calls = body_calls
            .iter()
            .filter(|symbol| {
                let facts = libraries.intrinsic_facts();
                match &symbol.signature {
                    KlibIrSignature::FileLocal { .. } => false,
                    KlibIrSignature::Public(signature) => {
                        let intrinsic = facts.get(&KlibDependencyCallable {
                            module: callable.identity().module.clone(),
                            declaration: KlibDeclarationSignature::Public(signature.clone()),
                        });
                        if intrinsic.is_some() {
                            assert_eq!(
                                signature.declaration().segments().len(),
                                1,
                                "an authenticated Wasm operation is top-level"
                            );
                            assert!(
                                signature.member_id().is_some(),
                                "a public callable IdSignature carries its mangled member id"
                            );
                        }
                        intrinsic.is_some()
                    }
                    KlibIrSignature::Accessor(_) => false,
                }
            })
            .count();
        assert_eq!(
            authenticated_calls,
            1,
            "{name} on {operand:?}; body calls: {body_calls:#?}; authenticated facts: {}",
            libraries.intrinsic_facts().len()
        );
        assert!(
            bodies.is_supported_intrinsic_root(callable.identity(), libraries.intrinsic_facts()),
            "{name} on {operand:?} is an authenticated Wasm intrinsic root"
        );
        let externals = intrinsic_externals(&libraries, callable.identity());
        let mut unit = DependencyBodyUnit::default();
        let function = unit
            .lower_function_with_intrinsics_and_externals(
                callable,
                bodies,
                &KlibCalleeFacts::default(),
                libraries.intrinsic_facts(),
                &externals,
            )
            .unwrap_or_else(|decline| panic!("{name} on {operand:?}: {decline}"));
        validated(unit.ir());
        assert_eq!(unit.ir().functions.len(), 1);
        assert_eq!(unit.ir().functions[function as usize].ret, result);
        if name == "fromBits" {
            let Ty::Obj(receiver, arguments) =
                fact.source_receiver.expect("fromBits is an extension")
            else {
                panic!("fromBits has a singleton classifier receiver")
            };
            assert!(arguments.is_empty());
            assert!(
                bodies.singleton_initialization_is_inert(&callable.identity().module, receiver),
                "the exact platform companion has no serialized initialization work"
            );
            assert!(
                !bodies.singleton_initialization_is_inert(
                    &callable.identity().module,
                    type_name("fixture/ObservableSingleton")
                ),
                "an absent classifier cannot grant inert-initialization authority"
            );
            assert_eq!(
                unit.ir().singleton_extension_unread_receivers[&function],
                fact.source_receiver.expect("fromBits is an extension")
            );
        }
        let calls = reinterpret_calls(&libraries, &externals, unit.ir(), operand, result);
        assert_eq!(calls, 1, "{name} on {operand:?}");
    }
}

#[test]
fn wasm_to_bits_closes_its_exact_semantic_wrapper_unit() {
    let Some(path) =
        crate::toolchain::kotlin_stdlib_klib(crate::compilation_target::CompilationTarget::WasmJs)
    else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "KRUSTY_REQUIRE_KLIB is set but no Wasm/JS stdlib KLIB is provisioned"
        );
        return;
    };
    let libraries =
        KlibLibraries::open(&[crate::klib_libraries::KlibLibraryInput::platform_stdlib(
            path,
            crate::klib_libraries::KlibStdlibSemantics::Wasm,
        )])
        .unwrap_or_else(|error| panic!("{error}"));
    let bodies = libraries.declaration_bodies();

    for (operand, result) in [(Ty::Float, Ty::Int), (Ty::Double, Ty::Long)] {
        let fact = frozen_function(&libraries, "kotlin", "toBits", Some(operand), &[]);
        let callable = fact.klib_body_callable().expect("signed");
        assert!(
            bodies.is_supported_intrinsic_root(callable.identity(), libraries.intrinsic_facts()),
            "toBits on {operand:?} is the one exact wrapper layer above the Wasm operation"
        );
        let externals = intrinsic_externals(&libraries, callable.identity());
        let mut unit = DependencyBodyUnit::default();
        let function = unit
            .lower_function_with_intrinsics_and_externals(
                callable,
                bodies,
                &KlibCalleeFacts::default(),
                libraries.intrinsic_facts(),
                &externals,
            )
            .unwrap_or_else(|decline| panic!("toBits on {operand:?}: {decline}"));
        validated(unit.ir());
        assert_eq!(unit.ir().functions[function as usize].ret, result);
        assert_eq!(
            reinterpret_calls(&libraries, &externals, unit.ir(), operand, result),
            1,
            "toBits on {operand:?} closes through exactly one authenticated reinterpret"
        );

        let nan = frozen_function(&libraries, "kotlin", "isNaN", Some(operand), &[]);
        let nan_callable = nan.klib_body_callable().expect("isNaN is signed");
        let selected = KlibCalleeFacts::new([nan_callable]);
        let mut selected_unit = DependencyBodyUnit::default();
        selected_unit
            .lower_function_with_intrinsics_and_externals(
                callable,
                bodies,
                &selected,
                libraries.intrinsic_facts(),
                &externals,
            )
            .unwrap_or_else(|decline| panic!("selected isNaN on {operand:?}: {decline}"));
        validated(selected_unit.ir());
        let closed = selected_unit
            .callable_functions()
            .map(|(identity, _)| identity)
            .collect::<Vec<_>>();
        assert_eq!(
            selected_unit.ir().functions.len(),
            3,
            "the 2.4.20 Wasm wrapper closes through its selected isNaN body and one exact transitive wrapper"
        );
        assert!(closed.contains(&callable.identity()));
        assert!(closed.contains(&nan_callable.identity()));
        assert_eq!(
            selected_unit
                .ir()
                .exprs
                .iter()
                .filter(|expression| matches!(
                    expression,
                    IrExpr::Equality {
                        mode: crate::types::EqualityMode::Ieee754,
                        ..
                    }
                ))
                .count(),
            1,
            "the selected isNaN body remains the exact IEEE predicate on {operand:?}"
        );
    }
}

#[test]
fn wasm_math_wrappers_are_scalar_roots_closing_through_one_instruction() {
    let Some(path) =
        crate::toolchain::kotlin_stdlib_klib(crate::compilation_target::CompilationTarget::WasmJs)
    else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "KRUSTY_REQUIRE_KLIB is set but no Wasm/JS stdlib KLIB is provisioned"
        );
        return;
    };
    let libraries =
        KlibLibraries::open(&[crate::klib_libraries::KlibLibraryInput::platform_stdlib(
            path,
            crate::klib_libraries::KlibStdlibSemantics::Wasm,
        )])
        .unwrap_or_else(|error| panic!("{error}"));
    let bodies = libraries.declaration_bodies();

    for (package, name, receiver, parameters, instruction, operand, result) in [
        (
            "kotlin/math",
            "sqrt",
            None,
            vec![Ty::Double],
            crate::libraries::ScalarInstruction::SquareRoot,
            Ty::Double,
            Ty::Double,
        ),
        (
            "kotlin/math",
            "min",
            None,
            vec![Ty::Float, Ty::Float],
            crate::libraries::ScalarInstruction::Minimum,
            Ty::Float,
            Ty::Float,
        ),
        (
            "kotlin/math",
            "withSign",
            Some(Ty::Double),
            vec![Ty::Double],
            crate::libraries::ScalarInstruction::CopySign,
            Ty::Double,
            Ty::Double,
        ),
        (
            "kotlin",
            "countLeadingZeroBits",
            Some(Ty::Long),
            vec![],
            crate::libraries::ScalarInstruction::CountLeadingZeros,
            Ty::Long,
            Ty::Long,
        ),
    ] {
        let fact = frozen_function(&libraries, package, name, receiver, &parameters);
        let callable = fact.klib_body_callable().expect("signed");
        assert!(
            DependencyBodyUnit::is_scalar_root_candidate(callable, bodies),
            "{name} has the scalar boundary of an instruction wrapper"
        );
        assert!(
            bodies.is_supported_intrinsic_root(callable.identity(), libraries.intrinsic_facts()),
            "{name} directly calls its authenticated instruction"
        );
        // The instruction stays an ordinary call of its declaration through the provider's
        // handle; only the target emitter replaces that call.
        let instructions = bodies
            .direct_callees(callable.identity())
            .into_iter()
            .filter(|callee| {
                libraries
                    .intrinsic_facts()
                    .get(callee)
                    .is_some_and(|intrinsic| {
                        intrinsic.operation()
                            == crate::libraries::CompilerIntrinsic::ScalarInstruction(instruction)
                            && intrinsic.params().iter().all(|ty| *ty == operand)
                            && intrinsic.ret() == result
                    })
            })
            .map(|callee| {
                let (external, _) = <KlibLibraries as crate::symbol_source::SymbolSource>::external_callable_by_dependency(
                        &libraries, &callee,
                    )
                    .unwrap_or_else(|| panic!("{name}: the provider publishes its instruction"));
                (callee, super::EmittedDependencyCallable::new(external, None, None))
            })
            .collect::<crate::fx_hash::HashMap<_, _>>();
        assert_eq!(
            instructions.len(),
            1,
            "{name} calls one authenticated instruction"
        );
        let target = instructions
            .values()
            .next()
            .expect("one instruction")
            .target();
        let mut unit = DependencyBodyUnit::default();
        unit.lower_function_with_intrinsics_and_externals(
            callable,
            bodies,
            &KlibCalleeFacts::default(),
            libraries.intrinsic_facts(),
            &instructions,
        )
        .unwrap_or_else(|decline| panic!("{name}: {decline}"));
        validated(unit.ir());
        assert_eq!(unit.ir().functions.len(), 1, "{name}");
        let calls = unit
            .ir()
            .exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::Call { callee, .. } => Some(callee),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            matches!(
                calls.as_slice(),
                [Callee::External { target: called, ret, .. }]
                    if *called == target && *ret == result
            ),
            "{name} closes through exactly one call of its instruction: {calls:?}"
        );
    }
}
