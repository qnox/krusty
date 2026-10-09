//! Whole declaration trees, bodies included, decoded from the Kotlin/Native stdlib KLIB.
//!
//! The native backend realizes stdlib calls against its own runtime today; compiling library code
//! instead needs the bodies the KLIB serializes. These tests pin that every one of them decodes,
//! and that a decoded body is the IR kotlinc serialized rather than an approximation of it.

use std::path::PathBuf;

use krusty::klib::KlibArchive;
use krusty::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrExprId, KlibIrExprKind, KlibIrStatement,
};
use krusty::metadata::klib_ir::{
    read_declaration_trees, KlibIrConstant, KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol,
};

fn distribution_root() -> Option<PathBuf> {
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if root.is_none() && std::env::var_os("KRUSTY_REQUIRE_KLIB").is_some() {
        panic!("KRUSTY_REQUIRE_KLIB is set but KRUSTY_KOTLIN_NATIVE is not");
    }
    root
}

fn stdlib_trees() -> Option<KlibIrModuleTrees> {
    let root = distribution_root()?;
    let stdlib = root.join("klib/common/stdlib");
    let archive = KlibArchive::open(&stdlib)
        .unwrap_or_else(|error| panic!("open {}: {error}", stdlib.display()));
    Some(
        read_declaration_trees(&archive)
            .unwrap_or_else(|error| panic!("decode {}: {error}", stdlib.display())),
    )
}

/// The one public function named `package.name` whose single regular parameter and extension
/// receiver both have the classifier `kotlin/Int`.
fn int_extension<'a>(
    trees: &'a KlibIrModuleTrees,
    package: &[&str],
    name: &str,
) -> (
    &'a KlibIrArena,
    &'a krusty::metadata::klib_ir::tree::KlibIrFunction,
) {
    let mut found = Vec::new();
    for tree in trees.trees() {
        for function in (0..tree.arena.function_count()).map(|index| tree.arena.function_at(index))
        {
            let KlibIrSignature::Public(signature) = &function.symbol.signature else {
                continue;
            };
            if signature.package().segments() != package
                || signature.declaration().segments() != [name]
            {
                continue;
            }
            let is_int = |ty| is_classifier(&tree.arena, ty, &["kotlin"], "Int");
            let receiver = function.extension_receiver.as_ref().map(|p| p.ty);
            if receiver.is_some_and(is_int)
                && function.regular_parameters.len() == 1
                && is_int(function.regular_parameters[0].ty)
            {
                found.push((&tree.arena, function));
            }
        }
    }
    assert_eq!(
        found.len(),
        1,
        "{name}: expected exactly one Int.{name}(Int)"
    );
    found.pop().unwrap()
}

fn is_classifier(
    arena: &KlibIrArena,
    ty: krusty::metadata::klib_ir::tree::KlibIrTypeId,
    package: &[&str],
    name: &str,
) -> bool {
    match arena.ty(ty) {
        krusty::metadata::klib_ir::tree::KlibIrType::Simple { classifier, .. } => {
            public_name(classifier) == Some((package.to_vec(), vec![name]))
        }
        _ => false,
    }
}

fn public_name(symbol: &KlibIrSymbol) -> Option<(Vec<&str>, Vec<&str>)> {
    let KlibIrSignature::Public(signature) = &symbol.signature else {
        return None;
    };
    Some((
        signature
            .package()
            .segments()
            .iter()
            .map(String::as_str)
            .collect(),
        signature
            .declaration()
            .segments()
            .iter()
            .map(String::as_str)
            .collect(),
    ))
}

/// A body rendered as nested calls, reads and literals: the shape a test can state exactly.
fn render(arena: &KlibIrArena, id: KlibIrExprId, parameters: &[&KlibIrSymbol]) -> String {
    let rendered = |id| render(arena, id, parameters);
    match &arena.expr(id).kind {
        KlibIrExprKind::Const(KlibIrConstant::Int(value)) => value.to_string(),
        KlibIrExprKind::Const(KlibIrConstant::Boolean(value)) => value.to_string(),
        KlibIrExprKind::GetValue { symbol, .. } => {
            match parameters.iter().position(|parameter| *parameter == symbol) {
                Some(index) => format!("p{index}"),
                None => "local".to_string(),
            }
        }
        KlibIrExprKind::Call { access, .. } => {
            let name = public_name(&access.symbol)
                .map(|(package, declaration)| {
                    format!("{}.{}", package.join("."), declaration.join("."))
                })
                .unwrap_or_else(|| "?".to_string());
            let KlibIrArguments::Flat(arguments) = &access.arguments else {
                return format!("{name}(<split>)");
            };
            let arguments = arguments
                .iter()
                .map(|argument| argument.map_or("_".to_string(), rendered))
                .collect::<Vec<_>>();
            format!("{name}({})", arguments.join(", "))
        }
        KlibIrExprKind::Return { value, .. } => format!("return {}", rendered(*value)),
        KlibIrExprKind::When { branches, .. } => {
            let branches = branches
                .iter()
                .map(|branch| {
                    format!(
                        "{} -> {}",
                        rendered(branch.condition),
                        rendered(branch.result)
                    )
                })
                .collect::<Vec<_>>();
            format!("when {{ {} }}", branches.join("; "))
        }
        other => format!("<{other:?}>"),
    }
}

#[test]
fn every_stdlib_declaration_tree_decodes() {
    let Some(trees) = stdlib_trees() else {
        eprintln!("KLIB IR integration requires KRUSTY_KOTLIN_NATIVE");
        return;
    };
    let mut bodies = 0usize;
    let mut expressions = 0usize;
    for tree in trees.trees() {
        expressions += tree.arena.expr_count();
        bodies += (0..tree.arena.function_count())
            .filter(|index| tree.arena.function_at(*index).body.is_some())
            .count();
    }
    eprintln!(
        "stdlib: {} trees, {} linkable functions, {bodies} bodies, {expressions} expressions",
        trees.trees().len(),
        trees.function_count()
    );
    assert!(trees.trees().len() > 1_000, "{} trees", trees.trees().len());
    assert!(
        trees.function_count() > 10_000,
        "{} functions",
        trees.function_count()
    );
    assert!(bodies > 10_000, "{bodies} bodies");
    assert!(expressions > 100_000, "{expressions} expressions");
}

#[test]
fn int_coerce_at_least_body_is_the_serialized_comparison() {
    let Some(trees) = stdlib_trees() else {
        eprintln!("KLIB IR integration requires KRUSTY_KOTLIN_NATIVE");
        return;
    };
    let (arena, function) = int_extension(&trees, &["kotlin", "ranges"], "coerceAtLeast");
    let receiver = &function.extension_receiver.as_ref().unwrap().symbol;
    let minimum = &function.regular_parameters[0].symbol;
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        panic!("coerceAtLeast has no block body: {:?}", function.body);
    };
    let rendered = statements
        .iter()
        .map(|statement| match statement {
            KlibIrStatement::Expression(id) => render(arena, *id, &[receiver, minimum]),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        ["return when { kotlin.internal.ir.less(p0, p1) -> p1; true -> p0 }"]
    );
}
