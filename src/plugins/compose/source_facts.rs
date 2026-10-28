//! What the Compose transform needs from a file's source text, recorded at the frontend/backend
//! handoff while that text is still live: each restartable function's durable key and offsets,
//! its source information and trace string, and the composable calls it makes. Every composable
//! construct the transform does not implement yet is rejected here, so no file compiles with an
//! untransformed composable in it.

use crate::fx_hash::HashMap;
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use crate::kotlinc_offsets::Utf16Offsets;
use crate::plugins::{PluginRejection, PluginSourceView};
use crate::types::{Ty, TyKind, TypeName};

use super::durable_keys::{package_hash, radix_string, KeyPath};
use super::{ComposeOptions, COMPOSABLE};

/// The Compose facts of one file.
pub struct ComposeFileFacts {
    pub(super) functions: Vec<RestartableFunction>,
}

/// A composable function the transform turns into a restartable group.
pub(super) struct RestartableFunction {
    pub(super) function: FunId,
    /// The durable group key, `DurableFunctionKeyTransformer`'s hash of the function's path.
    pub(super) key: i32,
    /// The declaration's UTF-16 start and end offsets, which `@FunctionKeyMeta` records.
    pub(super) offsets: (i32, i32),
    /// The `sourceInformation` string, when the option asks for it.
    pub(super) source_information: Option<String>,
    /// The `traceEventStart` string, when trace markers are enabled.
    pub(super) trace: Option<String>,
    /// Whether the function is annotated with its key.
    pub(super) key_meta: bool,
    /// The 1-based line of the body's opening brace, where the group starts and is skipped.
    pub(super) opening_line: u32,
    /// The 1-based line of the body's closing brace, where the group ends.
    pub(super) closing_line: u32,
    /// The function's direct composable calls, in source order.
    pub(super) calls: Vec<ExprId>,
}

/// Why a construct is outside what krusty's Compose pass compiles yet.
fn unsupported(what: &str) -> String {
    format!(
        "krusty's Compose compiler plugin does not support {what} yet; compile this module with \
         kotlinc"
    )
}

pub(super) fn record(
    ir: &mut IrFile,
    source: &PluginSourceView<'_>,
    options: &ComposeOptions,
    rejections: &mut Vec<PluginRejection>,
) {
    let composable = crate::types::type_name(COMPOSABLE);
    let composable_functions = ir
        .function_annotations
        .iter()
        .filter(|(_, annotations)| {
            annotations
                .applications()
                .any(|annotation| annotation.internal == composable)
        })
        .map(|(&function, _)| function)
        .collect::<crate::fx_hash::HashSet<_>>();
    reject_composable_types(ir, composable, rejections);
    let calls = composable_calls(ir, source, &composable_functions, composable);
    if composable_functions.is_empty() && calls.is_empty() {
        return;
    }
    let positions = Utf16Offsets::new(source.text);
    let package = ir.package.clone().unwrap_or_default().replace('/', ".");
    let file_reference = match package_hash(&package) {
        0 => source.file_name.to_string(),
        hash => format!("{}#{}", source.file_name, radix_string(hash, 36)),
    };
    let mut functions = Vec::new();
    let mut handled = crate::fx_hash::HashSet::default();
    for declaration in &ir.package_functions {
        if !composable_functions.contains(&declaration.function) {
            continue;
        }
        handled.insert(declaration.function);
        let span = declaration.signature_span;
        if let Some(what) = unsupported_declaration(declaration) {
            rejections.push(PluginRejection {
                span,
                message: unsupported(what),
            });
            continue;
        }
        let Some(body) = function_body(ir, declaration.function) else {
            rejections.push(PluginRejection {
                span,
                message: unsupported("a composable function without a block body"),
            });
            continue;
        };
        let Some(body_span) = source.expression_span(ir, body.block) else {
            rejections.push(PluginRejection {
                span,
                message: unsupported("a composable function whose body has no source position"),
            });
            continue;
        };
        if let Some(what) = unsupported_body_expression(ir, body.block) {
            rejections.push(PluginRejection {
                span,
                message: unsupported(what),
            });
            continue;
        }
        let direct = body
            .statements
            .iter()
            .copied()
            .filter(|statement| calls.contains_key(statement))
            .collect::<Vec<_>>();
        let mut locations = Vec::with_capacity(direct.len());
        for &call in &direct {
            let Some(call_span) = calls[&call] else {
                rejections.push(PluginRejection {
                    span,
                    message: unsupported("a composable call without a source position"),
                });
                continue;
            };
            let start = positions.unit(call_span.lo);
            let end = positions.unit(call_span.hi);
            locations.push(format!(
                "{}@{start}L{}",
                positions.line(call_span.lo),
                end - start
            ));
        }
        let name = ir
            .fn_source_names
            .get(&declaration.function)
            .cloned()
            .unwrap_or_else(|| declaration.name.clone());
        let qualified = if package.is_empty() {
            name.clone()
        } else {
            format!("{package}.{name}")
        };
        let signature = format!("{name}(){}", simple_type_name(declaration.ret));
        functions.push(RestartableFunction {
            function: declaration.function,
            key: KeyPath {
                signature: &signature,
                package: &package,
                file_name: source.file_name,
            }
            .key(),
            offsets: (
                positions.unit(span.lo) as i32,
                positions.unit(body_span.hi) as i32,
            ),
            source_information: options
                .source_information
                .then(|| format!("C({name}){}:{file_reference}", locations.join(","))),
            trace: options.trace_markers.then(|| {
                format!(
                    "{qualified} ({}:{})",
                    source.file_name,
                    positions.line(body_span.lo)
                )
            }),
            key_meta: options.function_key_meta,
            opening_line: positions.line(body_span.lo) + 1,
            closing_line: positions.line(body_span.hi.saturating_sub(1)) + 1,
            calls: direct,
        });
    }
    for (&function, _) in ir
        .function_annotations
        .iter()
        .filter(|(function, _)| composable_functions.contains(function))
    {
        if !handled.contains(&function) {
            rejections.push(PluginRejection {
                span: crate::diag::Span::new(0, 0),
                message: unsupported(&format!(
                    "the composable member or local function `{}`",
                    ir.functions[function as usize].name
                )),
            });
        }
    }
    // A composable call anywhere but directly in a restartable body needs a group of its own
    // (a branch, a loop, a lambda) or is kotlinc's own error (a non-composable caller).
    let direct = functions
        .iter()
        .flat_map(|function| function.calls.iter().copied())
        .collect::<crate::fx_hash::HashSet<_>>();
    let mut stray = calls
        .iter()
        .filter(|(call, _)| !direct.contains(call))
        .map(|(_, span)| span.unwrap_or(crate::diag::Span::new(0, 0)))
        .collect::<Vec<_>>();
    stray.sort_by_key(|span| (span.lo, span.hi));
    rejections.extend(stray.into_iter().map(|span| PluginRejection {
        span,
        message: unsupported("a composable call outside the statements of a composable body"),
    }));
    if !functions.is_empty() {
        ir.compose = Some(Box::new(ComposeFileFacts { functions }));
    }
}

/// The declaration shapes the restart-group transform does not handle yet.
fn unsupported_declaration(declaration: &crate::ir::IrPackageFunction) -> Option<&'static str> {
    if !declaration.params.is_empty() || declaration.receiver.is_some() {
        Some("a composable function with parameters")
    } else if !matches!(declaration.ret.kind(), TyKind::Unit) {
        Some("a composable function with a result")
    } else if declaration.inline {
        Some("an inline composable function")
    } else if declaration.suspend {
        Some("a suspend composable function")
    } else if !declaration.type_params.is_empty() {
        Some("a generic composable function")
    } else {
        None
    }
}

/// An expression in a composable body the transform does not handle yet: an early `return`, which
/// must end the group first, or a lambda, whose lifted name the group's own lambda would share.
fn unsupported_body_expression(ir: &IrFile, block: ExprId) -> Option<&'static str> {
    let mut pending = vec![block];
    while let Some(expression) = pending.pop() {
        match &ir.exprs[expression as usize] {
            IrExpr::Return(_) => return Some("a `return` in a composable function"),
            IrExpr::Lambda { .. } => return Some("a lambda in a composable function"),
            _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
    }
    None
}

/// A function's source block and its statements, the value-less last expression included.
pub(super) struct FunctionBody {
    pub(super) block: ExprId,
    pub(super) statements: Vec<ExprId>,
}

/// The source block common lowering wraps a function's statements in: the function's body is a
/// block whose one statement is the source `{ … }`.
pub(super) fn function_body(ir: &IrFile, function: FunId) -> Option<FunctionBody> {
    let root = ir.functions[function as usize].body?;
    let IrExpr::Block { stmts, value: None } = &ir.exprs[root as usize] else {
        return None;
    };
    let [block] = stmts.as_slice() else {
        return None;
    };
    let IrExpr::Block { stmts, value } = &ir.exprs[*block as usize] else {
        return None;
    };
    Some(FunctionBody {
        block: *block,
        statements: stmts.iter().copied().chain(*value).collect(),
    })
}

/// Every call in `ir` whose selected declaration is composable, with its source span.
fn composable_calls(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    composable_functions: &crate::fx_hash::HashSet<FunId>,
    composable: TypeName,
) -> HashMap<ExprId, Option<crate::diag::Span>> {
    let local = |function: &FunId| composable_functions.contains(function);
    let module = |target: &crate::fir::CallableId| {
        ir.checked_callable_functions
            .get(target)
            .is_some_and(|function| composable_functions.contains(function))
            || ir
                .referenced_module_callables
                .get(target)
                .is_some_and(|callable| {
                    callable
                        .annotations
                        .iter()
                        .any(|annotation| annotation.identity == composable)
                })
    };
    ir.exprs
        .iter()
        .enumerate()
        .filter(|(_, expression)| match expression {
            IrExpr::Call { callee, .. } => match &**callee {
                Callee::Local(function)
                | Callee::LocalDefault(function)
                | Callee::LocalWithDefaults { function, .. }
                | Callee::ClassStatic { function, .. }
                | Callee::ClassStaticWithDefaults { function, .. }
                | Callee::ClassStaticDefault { function, .. } => local(function),
                Callee::Module { target, .. } | Callee::ModuleWithDefaults { target, .. } => {
                    module(target)
                }
                Callee::CrossFile {
                    module_target: Some(target),
                    ..
                }
                | Callee::Virtual {
                    module_target: Some(target),
                    ..
                } => module(target),
                Callee::External { target, .. } => source
                    .callables
                    .callable(*target)
                    .is_some_and(|fact| fact.annotations.contains(&composable)),
                _ => false,
            },
            IrExpr::Checked(operation) => match &**operation {
                crate::ir::IrCheckedOperation::Call { target, .. } => module(target),
                _ => false,
            },
            _ => false,
        })
        .map(|(index, _)| {
            let call = index as ExprId;
            (call, source.expression_span(ir, call))
        })
        .collect()
}

/// Reject every declared type that names a composable function type: the calling convention of
/// such a value is not implemented yet.
fn reject_composable_types(
    ir: &IrFile,
    composable: TypeName,
    rejections: &mut Vec<PluginRejection>,
) {
    let names_composable =
        |spelled: &crate::spelling::Spelled| spelled_mentions(spelled, composable);
    let declared = ir
        .fn_declared_spellings
        .iter()
        .map(|(&function, spellings)| (ir.functions[function as usize].name.as_str(), spellings))
        .chain(
            ir.prop_declared_spellings
                .values()
                .map(|spellings| ("a property", spellings)),
        );
    let mut names = declared
        .filter(|(_, spellings)| {
            names_composable(&spellings.ret)
                || names_composable(&spellings.receiver)
                || spellings.params.iter().any(names_composable)
        })
        .map(|(name, _)| name.to_string())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    rejections.extend(names.into_iter().map(|name| PluginRejection {
        span: crate::diag::Span::new(0, 0),
        message: unsupported(&format!("the composable function type in `{name}`")),
    }));
}

fn spelled_mentions(spelled: &crate::spelling::Spelled, annotation: TypeName) -> bool {
    spelled
        .annotations
        .iter()
        .chain(&spelled.expansion_annotations)
        .any(|applied| matches!(applied, crate::spelling::TypeUseAnnotation::Checked(checked) if checked.annotation == annotation))
        || spelled
            .args
            .iter()
            .chain(spelled.alias_args.iter().map(|(_, spelled)| spelled))
            .any(|argument| spelled_mentions(argument, annotation))
}

/// `IrType.asString()` of a declared type: its classifier's simple name.
fn simple_type_name(ty: Ty) -> String {
    match ty.kind() {
        TyKind::Unit => "Unit".to_string(),
        TyKind::Nothing => "Nothing".to_string(),
        TyKind::TyParam(name, _) => name.as_str().to_string(),
        _ => ty
            .kotlin_class_internal()
            .map(|classifier| classifier.nested_segment_ref().to_string())
            .unwrap_or_default(),
    }
}
