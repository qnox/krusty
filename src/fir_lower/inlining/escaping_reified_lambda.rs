use crate::ir::{IrExpr, IrTypeOp};
use crate::types::Ty;

fn lower(source: &str, stem: &str) -> crate::ir::IrFile {
    stdlib::lower(source, stem)
}

/// The standard-library provider is a test fixture. Keeping it in this module lets the architecture
/// budget treat the rest of the file as checked lowering, which may not name a target.
#[cfg(test)]
mod stdlib {
    pub(super) fn lower(source: &str, stem: &str) -> crate::ir::IrFile {
        let platform: Box<dyn crate::libraries::SemanticPlatform> = Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
                crate::jvm::classpath::Classpath::new(crate::toolchain::classpath_jars_for(
                    "// WITH_STDLIB",
                )),
            ))
            .expect("JVM provider initialization"),
        );
        super::super::super::tests::lower_single_source_with_platform(source, stem, platform)
    }
}

fn function_named(ir: &crate::ir::IrFile, name: &str) -> u32 {
    ir.functions
        .iter()
        .position(|function| function.name == name)
        .unwrap_or_else(|| panic!("{name}")) as u32
}

fn callable_for_function(ir: &crate::ir::IrFile, function: u32) -> crate::fir::CallableId {
    ir.checked_callable_functions
        .iter()
        .find_map(|(&callable, &candidate)| (candidate == function).then_some(callable))
        .unwrap_or_else(|| panic!("function {function} has no checked callable identity"))
}

fn lambda_implementations(ir: &crate::ir::IrFile, function: u32) -> Vec<u32> {
    let body = ir.functions[function as usize]
        .body
        .unwrap_or_else(|| panic!("{function} has a body"));
    let mut pending = vec![body];
    let mut seen_exprs = std::collections::HashSet::new();
    let mut seen_functions = std::collections::HashSet::new();
    let mut implementations = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen_exprs.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.expr(expression) {
            if seen_functions.insert(*impl_fn) {
                implementations.push(*impl_fn);
                if let Some(body) = ir.functions[*impl_fn as usize].body {
                    pending.push(body);
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    implementations
}

fn instanceof_targets(ir: &crate::ir::IrFile, function: u32) -> Vec<Ty> {
    let Some(body) = ir.functions[function as usize].body else {
        return Vec::new();
    };
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut targets = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            type_operand,
            ..
        } = ir.expr(expression)
        {
            targets.push(*type_operand);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    targets
}

fn is_token(ty: Ty) -> bool {
    ty.non_null() == Ty::obj("Token")
}

const REIFIED: &str = "\
interface Item\n\
class Token : Item\n\
class Root : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun box() {\n\
    defineFunc<Token>()\n\
    check(Token())\n\
    check(Root())\n\
}\n";

#[test]
fn an_inlined_reified_check_specializes_only_the_call_site_lambda() {
    let ir = lower(REIFIED, "ReifiedEscapingLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineFunc"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    assert_eq!(declaration.len(), 1, "the declaration stores one lambda");
    assert_eq!(call.len(), 1, "the call installs one lambda");
    assert_ne!(declaration[0], call[0]);
    assert!(
        instanceof_targets(&ir, declaration[0])
            .iter()
            .any(|ty| matches!(ty, Ty::TyParam(..))),
        "the declaration implementation keeps the reified parameter"
    );
    assert!(
        instanceof_targets(&ir, call[0])
            .iter()
            .any(|ty| is_token(*ty)),
        "the call-site lambda tests Token"
    );
    let specialization = ir
        .specialized_functions
        .get(&call[0])
        .expect("the call-site lambda records its expansion");
    assert_eq!(specialization.source, declaration[0]);
    assert_eq!(
        specialization.caller,
        Some(crate::ir::IrEnclosure::Function(function_named(&ir, "box")))
    );
    assert_eq!(specialization.caller_source_name, "box");
    assert_eq!(
        specialization.inline_callee,
        callable_for_function(&ir, function_named(&ir, "defineFunc"))
    );
    assert_eq!(specialization.inline_callee_source_name, "defineFunc");
    assert_eq!(specialization.parent, None);
    assert_eq!(
        ir.lambda_origins.get(&declaration[0]),
        ir.lambda_origins.get(&call[0]),
        "the copy keeps the source lambda's identity"
    );
    assert_copy_contract(&ir, declaration[0], call[0]);
}

/// Facts the clone must copy, substitute, or leave on the declaration.
///
/// A new function-keyed table has to be classified here. The copy is another implementation of
/// the same lambda, not a second declaration and not a backend realization of the source.
fn assert_copy_contract(ir: &crate::ir::IrFile, source: u32, copy: u32) {
    assert_eq!(ir.lambda_origins.get(&source), ir.lambda_origins.get(&copy));
    assert_eq!(
        ir.fn_source_names.get(&source),
        ir.fn_source_names.get(&copy)
    );
    assert_eq!(
        ir.method_visibility(source),
        ir.method_visibility(copy),
        "visibility is an implementation fact"
    );
    assert!(ir.specialized_functions.contains_key(&copy));
    assert!(!ir.specialized_functions.contains_key(&source));
    assert!(ir.runtime_reified_lambda_implementations.contains(&source));
    assert!(ir.runtime_reified_lambda_implementations.contains(&copy));
    assert!(
        ir.checked_callable_functions
            .values()
            .all(|function| *function != copy),
        "the callable map names the declaration"
    );
    for table in [
        &ir.inline_fns,
        &ir.public_inline_functions,
        &ir.top_level_inline_functions,
        &ir.foreign_inline_templates,
        &ir.function_reference_access_bridges,
        &ir.interface_delegation_forwarders,
    ] {
        assert!(
            !table.contains(&copy),
            "declaration sets stay on the source implementation"
        );
    }
    assert!(!ir.lambda_class_names.contains_key(&copy));
    assert!(!ir.lambda_sam_jvm_signature.contains_key(&copy));
    assert!(!ir.default_stub_boxed_params.contains_key(&copy));
    assert!(!ir.vc_declared_sigs.contains_key(&copy));
    if let Some(signature) = ir.signatures.get(&copy) {
        let mentions_param = signature
            .params
            .iter()
            .chain(signature.ret.iter())
            .any(|ty| matches!(ty, Ty::TyParam(..)));
        assert!(
            !mentions_param,
            "the copy's signature uses the call's type arguments: {signature:?}"
        );
    }
}

const NESTED: &str = "\
interface Item\n\
class Token : Item\n\
class Root : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = {\n\
        val nested = { value: Item -> value is T }\n\
        nested(it)\n\
    }\n\
}\n\
fun box() {\n\
    defineFunc<Token>()\n\
    check(Token())\n\
}\n";

#[test]
fn a_nested_escaping_lambda_is_specialized_too() {
    let ir = lower(NESTED, "NestedReifiedEscapingLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineFunc"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    assert_eq!(declaration.len(), 2, "declaration outer and nested lambdas");
    assert_eq!(call.len(), 2, "call-site outer and nested lambdas");
    assert!(declaration.iter().all(|function| !call.contains(function)));
    let outer = call
        .iter()
        .copied()
        .find(|function| ir.specialized_functions[function].parent.is_none())
        .expect("the expansion has one top-level specialized lambda");
    let nested = call
        .iter()
        .copied()
        .find(|function| ir.specialized_functions[function].parent == Some(outer))
        .expect("the nested specialization retains its lexical parent");
    assert_ne!(outer, nested);
    assert!(
        declaration.iter().any(|function| {
            instanceof_targets(&ir, *function)
                .iter()
                .any(|ty| matches!(ty, Ty::TyParam(..)))
        }),
        "the nested declaration implementation keeps the reified parameter"
    );
    assert!(
        call.iter().any(|function| {
            instanceof_targets(&ir, *function)
                .iter()
                .any(|ty| is_token(*ty))
        }),
        "the nested call-site lambda tests Token"
    );
    assert_eq!(
        declaration
            .iter()
            .filter(|function| ir
                .runtime_reified_lambda_implementations
                .contains(*function))
            .count(),
        1,
        "only the outer source closure needs a runtime-reified class"
    );
    assert!(
        call.iter()
            .all(|function| ir.runtime_reified_lambda_implementations.contains(function)),
        "each specialized closure needs its own runtime-reified class"
    );
}

const ORDINARY: &str = "\
class Token\n\
var latest: () -> Token? = { null }\n\
inline fun <T> defineFunc(value: T) {\n\
    latest = { value as? Token }\n\
}\n\
fun box() {\n\
    defineFunc(Token())\n\
    latest()\n\
}\n";

#[test]
fn an_escaping_lambda_that_forwards_a_reified_argument_is_specialized() {
    let ir = lower(FORWARDED, "ForwardedReifiedLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineFunc"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    assert_eq!(declaration.len(), 1, "the declaration stores one lambda");
    assert_eq!(call.len(), 1, "the call installs one lambda");
    assert_ne!(
        declaration[0], call[0],
        "forwarding T must not reuse the declaration implementation"
    );
    let declaration_checks = instanceof_targets(&ir, declaration[0]);
    let call_checks = instanceof_targets(&ir, call[0]);
    assert!(
        declaration_checks
            .iter()
            .any(|ty| matches!(ty, Ty::TyParam(..))),
        "inlining accept leaves the declaration check on the reified parameter: {declaration_checks:?}"
    );
    assert!(
        call_checks.iter().any(|ty| is_token(*ty)),
        "inlining accept leaves the call-site check on Token: {call_checks:?}"
    );
    assert!(
        call_checks.iter().all(|ty| !matches!(ty, Ty::TyParam(..))),
        "the call-site check does not keep the erased parameter: {call_checks:?}"
    );
}

fn forwarded_type_arguments(ir: &crate::ir::IrFile, function: u32) -> Vec<Ty> {
    let Some(body) = ir.functions[function as usize].body else {
        return Vec::new();
    };
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut arguments = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let Some(substitutions) = ir.reified_call_subst.get(&expression) {
            arguments.extend(substitutions.iter().map(|(_, ty)| *ty));
        }
        match ir.expr(expression) {
            IrExpr::Call {
                callee: crate::ir::Callee::External { substitutions, .. },
                ..
            } => arguments.extend(substitutions.iter().map(|substitution| substitution.value)),
            IrExpr::Checked(crate::ir::IrCheckedOperation::Call { substitutions, .. }) => {
                arguments.extend(substitutions.iter().map(|substitution| substitution.value))
            }
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    arguments
}

const FORWARDED: &str = "\
interface Item\n\
class Token : Item\n\
class Root : Item\n\
var accepted: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> accept(value: Item): Boolean = value is T\n\
inline fun <reified T : Item> defineFunc() {\n\
    accepted = { accept<T>(it) }\n\
}\n\
fun box() {\n\
    defineFunc<Token>()\n\
    accepted(Token())\n\
    accepted(Root())\n\
}\n";

const TYPE_OF: &str = "\
import kotlin.reflect.typeOf\n\
interface Item\n\
class Token : Item\n\
var kind: () -> Any? = { null }\n\
inline fun <reified T : Item> defineFunc() {\n\
    kind = { typeOf<T>() }\n\
}\n\
fun box() {\n\
    defineFunc<Token>()\n\
    kind()\n\
}\n";

#[test]
fn an_escaping_lambda_that_only_uses_type_of_is_specialized() {
    let ir = lower(TYPE_OF, "TypeOfReifiedLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineFunc"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    assert_eq!(declaration.len(), 1, "the declaration stores one lambda");
    assert_eq!(call.len(), 1, "the call installs one lambda");
    assert_ne!(declaration[0], call[0]);
    let declaration_arguments = forwarded_type_arguments(&ir, declaration[0]);
    let call_arguments = forwarded_type_arguments(&ir, call[0]);
    assert!(
        type_of_arguments(&ir, declaration[0]).is_empty()
            && type_of_arguments(&ir, call[0]).is_empty(),
        "common IR keeps typeOf as the call's type argument, not an intrinsic"
    );
    assert!(
        declaration_arguments
            .iter()
            .any(|ty| matches!(ty, Ty::TyParam(..))),
        "the declaration typeOf argument is the reified parameter: {declaration_arguments:?}"
    );
    assert!(
        call_arguments.iter().any(|ty| is_token(*ty)),
        "the call-site typeOf argument is Token: {call_arguments:?}"
    );
    assert!(
        call_arguments
            .iter()
            .all(|ty| !matches!(ty, Ty::TyParam(..))),
        "the call-site typeOf argument is not left as a type parameter: {call_arguments:?}"
    );
    assert!(
        instanceof_targets(&ir, call[0]).is_empty(),
        "typeOf is not an instanceof carrier: {:?}",
        instanceof_targets(&ir, call[0])
    );
}

fn type_of_arguments(ir: &crate::ir::IrFile, function: u32) -> Vec<Ty> {
    let Some(body) = ir.functions[function as usize].body else {
        return Vec::new();
    };
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut arguments = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Call {
            callee: crate::ir::Callee::Intrinsic { operation, .. },
            ..
        } = ir.expr(expression)
        {
            if let crate::ir::IrIntrinsic::TypeOf { ty } = operation {
                arguments.push(*ty);
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    arguments
}

const NESTED_ORDINARY: &str = "\
class Token\n\
var latest: () -> Token? = { null }\n\
inline fun <T> defineFunc(value: T) {\n\
    latest = {\n\
        val nested = { value as? Token }\n\
        nested()\n\
    }\n\
}\n\
fun box() {\n\
    defineFunc(Token())\n\
    latest()\n\
}\n";

#[test]
fn a_nested_ordinary_lambda_is_not_copied() {
    let ir = lower(NESTED_ORDINARY, "NestedOrdinaryEscapingLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineFunc"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    assert_eq!(declaration.len(), 2, "declaration outer and nested lambdas");
    assert_eq!(call.len(), 2, "the call reuses both declaration lambdas");
    assert!(declaration.iter().all(|function| call.contains(function)));
    assert!(
        ir.specialized_functions.is_empty(),
        "an ordinary type parameter does not copy nested implementations"
    );
}

#[test]
fn an_ordinary_type_parameter_keeps_one_shared_lambda() {
    let ir = lower(ORDINARY, "OrdinaryEscapingLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineFunc"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    let shared = declaration
        .iter()
        .copied()
        .find(|function| call.contains(function))
        .expect("the call reuses the declaration lambda");
    assert!(
        ir.specialized_functions.get(&shared).is_none(),
        "an ordinary type parameter does not create a second implementation"
    );
}

const STATIC_ONLY_REIFIED: &str = "\
interface Item\n\
class Token : Item\n\
var latest: () -> Item? = { null }\n\
inline fun <reified T : Item> defineStatic(value: T) {\n\
    latest = { value }\n\
}\n\
fun box() {\n\
    defineStatic(Token())\n\
    latest()\n\
}\n";

#[test]
fn static_type_facts_do_not_copy_an_escaping_lambda() {
    let ir = lower(STATIC_ONLY_REIFIED, "StaticOnlyReifiedLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineStatic"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    assert_eq!(declaration.len(), 1, "the declaration stores one lambda");
    assert_eq!(call, declaration, "the call reuses that implementation");
    assert!(
        ir.specialized_functions.is_empty(),
        "a captured value's static T facts are not a runtime reification use"
    );
}

const MIXED: &str = "\
interface Item\n\
class Token : Item\n\
class Root : Item\n\
var f: (Any) -> Any? = { null }\n\
inline fun <T, reified R : Item> define() {\n\
    f = { value -> if (value is R) value as? T else null }\n\
}\n\
fun box() {\n\
    define<Root, Token>()\n\
    f(Token())\n\
}\n";

fn type_operands(ir: &crate::ir::IrFile, function: u32) -> Vec<Ty> {
    let Some(body) = ir.functions[function as usize].body else {
        return Vec::new();
    };
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut operands = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::TypeOp { type_operand, .. } = ir.expr(expression) {
            operands.push(*type_operand);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    operands
}

fn is_root(ty: Ty) -> bool {
    ty.non_null() == Ty::obj("Root")
}

#[test]
fn a_reified_sibling_does_not_reify_an_ordinary_parameter() {
    let ir = lower(MIXED, "MixedReifiedLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "define"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    assert_eq!(declaration.len(), 1);
    assert_eq!(call.len(), 1);
    assert_ne!(declaration[0], call[0]);
    let declaration_operands = type_operands(&ir, declaration[0]);
    let call_operands = type_operands(&ir, call[0]);
    assert!(
        declaration_operands
            .iter()
            .filter(|ty| matches!(ty, Ty::TyParam(..)))
            .count()
            >= 2,
        "the declaration keeps both type parameters: {declaration_operands:?}"
    );
    assert!(
        call_operands.iter().any(|ty| is_token(*ty)),
        "the reified check becomes Token: {call_operands:?}"
    );
    assert!(
        call_operands.iter().any(|ty| matches!(ty, Ty::TyParam(..))),
        "the ordinary cast stays a type parameter: {call_operands:?}"
    );
    assert!(
        call_operands.iter().all(|ty| !is_root(*ty)),
        "the ordinary parameter is not specialized to Root: {call_operands:?}"
    );
}

const REPEATED: &str = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun box() {\n\
    defineFunc<Token>()\n\
    defineFunc<Token>()\n\
}\n";

#[test]
fn repeated_expansions_keep_distinct_semantic_copies_without_physical_ordinals() {
    let ir = lower(REPEATED, "RepeatedReifiedLambda");
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineFunc"));
    let call = lambda_implementations(&ir, function_named(&ir, "box"));
    assert_eq!(
        declaration.len(),
        1,
        "one declaration lambda is copied twice"
    );
    assert_eq!(call.len(), 2, "each expansion installs its own lambda");
    for function in call {
        let specialization = ir
            .specialized_functions
            .get(&function)
            .expect("each copy records its expansion");
        assert_eq!(
            specialization.caller,
            Some(crate::ir::IrEnclosure::Function(function_named(&ir, "box")))
        );
        assert_eq!(specialization.caller_source_name, "box");
        assert_eq!(
            specialization.inline_callee,
            callable_for_function(&ir, function_named(&ir, "defineFunc"))
        );
        assert_eq!(specialization.inline_callee_source_name, "defineFunc");
        assert_eq!(specialization.source, declaration[0]);
        assert_eq!(specialization.parent, None);
    }
}

#[test]
fn ordinary_lambda_is_the_expansion_caller_without_becoming_a_declaration_scope() {
    let ir = lower(
        "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun outer() {\n\
    val caller = {\n\
        class Local\n\
        defineFunc<Token>()\n\
    }\n\
    caller()\n\
}\n",
        "OrdinaryLambdaExpansionCaller",
    );
    let outer = function_named(&ir, "outer");
    let implementations = lambda_implementations(&ir, outer);
    let (&copy, specialization) = ir
        .specialized_functions
        .iter()
        .find(|(function, _)| implementations.contains(function))
        .expect("the inline expansion installs a specialized lambda");
    let crate::ir::IrEnclosure::Lambda(caller) =
        specialization.caller.expect("the expansion has a caller")
    else {
        panic!("an expansion in an ordinary lambda records that exact lambda");
    };
    assert!(implementations.contains(&caller));
    assert_ne!(caller, copy);
    let local = ir
        .classes
        .iter()
        .find(|class| class.is_local_class)
        .expect("the caller lambda declares one local class");
    assert_eq!(
        local.enclosure,
        Some(crate::ir::IrEnclosure::Function(outer)),
        "a plain lambda remains transparent to declaration scoping"
    );
}

#[test]
fn a_specialized_suspend_lambda_keeps_class_provenance_at_its_call_site() {
    let ir = lower(
        "\
interface Item\n\
class Token : Item\n\
var check: suspend (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun install() {\n\
    defineFunc<Token>()\n\
}\n",
        "SuspendEscapingReifiedLambda",
    );
    let declaration = lambda_implementations(&ir, function_named(&ir, "defineFunc"));
    let call = lambda_implementations(&ir, function_named(&ir, "install"));
    assert_eq!(declaration.len(), 1);
    assert_eq!(call.len(), 1);
    assert_ne!(declaration, call);
    assert!(ir.suspend_funs.contains(&call[0]));
    let expression_for = |implementation| {
        ir.exprs
            .iter()
            .position(
                |expression| matches!(expression, IrExpr::Lambda { impl_fn, .. } if *impl_fn == implementation),
            )
            .expect("the implementation has one lambda value") as u32
    };
    let declaration_expression = expression_for(declaration[0]);
    let call_expression = expression_for(call[0]);
    assert_eq!(
        ir.callable_reference_provenance
            .get(&call_expression)
            .expect("the specialized suspend lambda keeps source naming provenance"),
        ir.callable_reference_provenance
            .get(&declaration_expression)
            .expect("the declaration suspend lambda has source naming provenance")
    );
    assert_eq!(
        ir.callable_reference_enclosures.get(&call_expression),
        Some(&crate::ir::IrEnclosure::Function(function_named(
            &ir, "install"
        )))
    );
}
