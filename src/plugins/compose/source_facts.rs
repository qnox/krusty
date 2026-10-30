//! What the Compose transform needs from a file's source text, recorded at the frontend/backend
//! handoff while that text is still live: each restartable function's durable key and offsets,
//! its source information and trace string, and the composable calls it makes. Every composable
//! construct the transform does not implement yet is rejected here, so no file compiles with an
//! untransformed composable in it.

use crate::diag::kotlinc_offsets::Utf16Offsets;
use crate::fx_hash::HashMap;
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use crate::plugins::runtime_declarations::PluginRuntime;
use crate::plugins::{PluginRejection, PluginSourceView};
use crate::types::{Ty, TyKind, TypeName};

use super::applier_targets::{self, LambdaCall, TargetConstraints};
use super::durable_keys::{package_hash, radix_string, KeyPath};
use super::runtime::ComposeRuntime;
use super::{ComposeOptions, COMPOSABLE};

/// The Compose facts of one file.
pub struct ComposeFileFacts {
    pub(super) functions: Vec<RestartableFunction>,
    pub(super) runtime: ComposeRuntime,
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
    /// The applier scheme inferred for its composable lambda parameters, which
    /// `@ComposableInferredTarget` records.
    pub(super) inferred_target: Option<String>,
    /// The 1-based line of the body's opening brace, where the group starts.
    pub(super) opening_line: u32,
    /// The 1-based line of the `fun` keyword, where the group is skipped.
    pub(super) declaration_line: u32,
    /// The 1-based line of the body's closing brace, where the group ends.
    pub(super) closing_line: u32,
    /// The function's value parameters, which its `$changed` bits track slot by slot.
    pub(super) parameters: Vec<TrackedParameter>,
    /// The function's direct composable calls, in source order.
    pub(super) calls: Vec<ComposableCallSite>,
    /// The lifting sequence the function's recompose lambda is numbered in, and the function's
    /// source order among the file's declarations.
    pub(super) lifting: (crate::ir::IrLiftingSequence, u32),
}

/// One value parameter of a restartable function.
pub(super) struct TrackedParameter {
    pub(super) name: String,
    pub(super) ty: Ty,
    /// Whether the body reads it, which is when the function compares it to decide on skipping.
    pub(super) used: bool,
    /// Its default value, a constant the group assigns when the caller omits the argument.
    pub(super) default: Option<ExprId>,
    /// Whether it is a composable `() -> Unit` value, which takes the caller's composer when called
    /// and is compared by identity.
    pub(super) lambda: bool,
}

/// A composable call directly in a restartable body, and what the caller knows about each argument
/// it passes, which becomes the call's `$changed` bits.
pub(super) struct ComposableCallSite {
    /// A call of a composable function, or an `invoke` of a composable parameter.
    pub(super) call: ExprId,
    /// The callee's parameters in order, the omitted ones included; an `invoke`'s one slot is the
    /// called value itself.
    pub(super) arguments: Vec<ArgumentState>,
}

/// What kotlinc's `argumentMetaOf` finds about one argument of a composable call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArgumentState {
    /// A constant, the same in every execution.
    Static,
    /// The caller's own parameter in this slot, whose state the caller's `$dirty` holds.
    Forwarded(u32),
    /// Anything else the caller computes, which the callee compares itself.
    Uncertain,
    /// An argument the call omits, which the callee's default supplies.
    Omitted,
}

/// The most value parameters one `$changed` int tracks; a function with more takes several.
const SLOTS_PER_INT: usize = 10;

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
    runtime: &mut dyn PluginRuntime,
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
    let package_functions = ir
        .package_functions
        .iter()
        .map(|declaration| declaration.function)
        .collect::<crate::fx_hash::HashSet<_>>();
    reject_composable_types(
        ir,
        composable,
        |function, index| {
            composable_functions.contains(&function)
                && package_functions.contains(&function)
                && is_composable_lambda(ir, function, index, composable)
        },
        rejections,
    );
    reject_stability_inferred_classes(ir, source, rejections);
    if ir.file_annotations.applications().any(|annotation| {
        GROUP_ANNOTATIONS[4..]
            .iter()
            .any(|&(target, _)| annotation.internal == crate::types::type_name(target))
    }) {
        rejections.push(PluginRejection {
            span: crate::diag::Span::new(0, 0),
            message: unsupported("a file-level applier target"),
        });
    }
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
    let mut targets = HashMap::default();
    let mut handled = crate::fx_hash::HashSet::default();
    // Every call directly in a restartable body, a rejected one included, which needs no second
    // rejection as a stray call.
    let mut direct_calls = crate::fx_hash::HashSet::default();
    for declaration in &ir.package_functions {
        if !composable_functions.contains(&declaration.function) {
            continue;
        }
        handled.insert(declaration.function);
        let span = declaration.signature_span;
        let lambdas = (0..declaration.params.len())
            .map(|index| is_composable_lambda(ir, declaration.function, index, composable))
            .collect::<Vec<_>>();
        if let Some(what) = unsupported_group_annotation(ir, declaration.function)
            .or_else(|| unsupported_declaration(declaration, &lambdas))
            .or_else(|| unsupported_defaults(ir, declaration.function))
            .or_else(|| {
                (options.source_information
                    && !declaration.params.is_empty()
                    && !runtime.declares_classifier(crate::types::type_name(SOURCE_INFORMATION)))
                .then_some(
                    "the source information of a composable function with parameters for a \
                     runtime that predates parameter names in it",
                )
            })
        {
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
        // The recompose lambda is the only callable lifted along the function's sequence; one that
        // also numbers source callables (another overload's) is not placed yet.
        let lifting = crate::ir::IrLiftingSequence {
            source: source.source,
            owner: "file".into(),
            container: declaration.name.as_str().into(),
        };
        if ir.lifts_along(&lifting) {
            rejections.push(PluginRejection {
                span,
                message: unsupported(
                    "a composable function whose name an overload with lambdas shares",
                ),
            });
            continue;
        }
        let parameter_count = declaration.params.len() as u32;
        let invoked = |statement: ExprId| match &ir.exprs[statement as usize] {
            IrExpr::InvokeFunction { func, .. } => match ir.exprs[*func as usize] {
                IrExpr::GetValue(index) if index < parameter_count && lambdas[index as usize] => {
                    Some(index)
                }
                _ => None,
            },
            _ => None,
        };
        let direct = body
            .statements
            .iter()
            .copied()
            .filter(|&statement| calls.contains_key(&statement) || invoked(statement).is_some())
            .collect::<Vec<_>>();
        direct_calls.extend(direct.iter().copied());
        if let Some(what) = unsupported_lambda_use(ir, body.block, &direct, &lambdas) {
            rejections.push(PluginRejection {
                span,
                message: unsupported(what),
            });
            continue;
        }
        let mut sites = Vec::with_capacity(direct.len());
        let mut locations = Vec::with_capacity(direct.len());
        for &call in &direct {
            if let Some(index) = invoked(call) {
                let Some(call_span) = source.expression_span(ir, call) else {
                    rejections.push(PluginRejection {
                        span,
                        message: unsupported("a composable call without a source position"),
                    });
                    continue;
                };
                sites.push(ComposableCallSite {
                    call,
                    arguments: vec![ArgumentState::Forwarded(index)],
                });
                locations.push(call_location(&positions, call_span));
                continue;
            }
            if !calls[&call].same_file {
                rejections.push(PluginRejection {
                    span: calls[&call].span.unwrap_or(span),
                    message: unsupported("a call to a composable function from another file"),
                });
                continue;
            }
            let Some(call_span) = calls[&call].span else {
                rejections.push(PluginRejection {
                    span,
                    message: unsupported("a composable call without a source position"),
                });
                continue;
            };
            let IrExpr::Call { callee, args, .. } = &ir.exprs[call as usize] else {
                unreachable!("a composable call is a call");
            };
            let omitted: &[u32] = match &**callee {
                Callee::LocalWithDefaults { defaults, .. } => defaults,
                _ => &[],
            };
            let mut supplied = args.iter();
            let arguments = (0..(args.len() + omitted.len()) as u32)
                .map(|ordinal| {
                    if omitted.contains(&ordinal) {
                        return Some(ArgumentState::Omitted);
                    }
                    let argument = *supplied.next()?;
                    argument_state(ir, argument, parameter_count)
                })
                .collect::<Option<Vec<_>>>();
            let Some(arguments) = arguments else {
                rejections.push(PluginRejection {
                    span: call_span,
                    message: unsupported(
                        "a composable call argument that is not a constant, a parameter, or \
                         computed from a parameter",
                    ),
                });
                continue;
            };
            sites.push(ComposableCallSite { call, arguments });
            locations.push(call_location(&positions, call_span));
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
        if lambdas.contains(&true) {
            targets.insert(
                declaration.function,
                target_constraints(ir, &lambdas, &sites),
            );
        }
        let used = used_parameters(ir, body.block, parameter_count);
        let defaults = ir
            .param_defaults(declaration.function)
            .cloned()
            .unwrap_or_default();
        let parameters = (0..)
            .zip(&declaration.params)
            .zip(used)
            .map(|((index, (name, ty)), used)| TrackedParameter {
                name: name.clone(),
                ty: *ty,
                used,
                default: defaults.get(index).copied().flatten(),
                lambda: lambdas[index],
            })
            .collect::<Vec<_>>();
        let signature = format!(
            "{name}({}){}",
            parameters
                .iter()
                .map(|parameter| {
                    // The frontend's type for a composable `() -> Unit` value.
                    if parameter.lambda {
                        "ComposableFunction0".to_string()
                    } else {
                        simple_type_name(parameter.ty)
                    }
                })
                .collect::<Vec<_>>()
                .join(","),
            simple_type_name(declaration.ret)
        );
        // `parameterNameInformation`: the source parameters' names, which the runtime's tooling
        // maps the tracked slots back to.
        let names = if parameters.is_empty() {
            String::new()
        } else {
            format!(
                "N({})",
                parameters
                    .iter()
                    .map(|parameter| parameter.name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
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
                .then(|| format!("C({name}){names}{}:{file_reference}", locations.join(","))),
            trace: options.trace_markers.then(|| {
                format!(
                    "{qualified} ({}:{})",
                    source.file_name,
                    positions.line(body_span.lo)
                )
            }),
            key_meta: options.function_key_meta,
            inferred_target: None,
            opening_line: positions.line(body_span.lo) + 1,
            declaration_line: positions.line(span.lo) + 1,
            closing_line: positions.line(body_span.hi.saturating_sub(1)) + 1,
            parameters,
            calls: sites,
            lifting: (lifting, declaration.source_order),
        });
    }
    // A lambda passed to a rejected function constrains nothing the file still compiles.
    let constrained = targets
        .keys()
        .copied()
        .collect::<crate::fx_hash::HashSet<_>>();
    for constraints in targets.values_mut() {
        constraints
            .calls
            .retain(|call| constrained.contains(&call.callee));
    }
    match applier_targets::infer(&targets) {
        Ok(schemes) => {
            for function in &mut functions {
                function.inferred_target = schemes
                    .get(&function.function)
                    .and_then(applier_targets::Scheme::inferred);
            }
        }
        Err(recursive) => rejections.push(PluginRejection {
            span: ir
                .package_functions
                .iter()
                .find(|declaration| declaration.function == recursive)
                .map_or(crate::diag::Span::new(0, 0), |declaration| {
                    declaration.signature_span
                }),
            message: unsupported(
                "a recursive composable function whose lambda parameters' applier kotlinc infers",
            ),
        }),
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
    let mut stray = calls
        .iter()
        .filter(|(call, _)| !direct_calls.contains(call))
        .map(|(_, call)| call.span.unwrap_or(crate::diag::Span::new(0, 0)))
        .collect::<Vec<_>>();
    stray.sort_by_key(|span| (span.lo, span.hi));
    // A lambda's body is lowered twice, once for its inline form, so a call in it is met twice.
    stray.dedup();
    rejections.extend(stray.into_iter().map(|span| PluginRejection {
        span,
        message: unsupported("a composable call outside the statements of a composable body"),
    }));
    let Some(first) = functions.first() else {
        return;
    };
    match ComposeRuntime::bind(runtime) {
        Ok(runtime) => ir.compose = Some(Box::new(ComposeFileFacts { functions, runtime })),
        Err(missing) => {
            let span = ir
                .package_functions
                .iter()
                .find(|declaration| declaration.function == first.function)
                .map_or(crate::diag::Span::new(0, 0), |declaration| {
                    declaration.signature_span
                });
            rejections.push(PluginRejection {
                span,
                message: format!(
                    "krusty's Compose compiler plugin cannot compile this module: {missing}"
                ),
            });
        }
    }
}

/// kotlinc's plugin gives every class of a Compose module, composable or not, a `$stable` field
/// and `@StabilityInferred`; krusty's pass does not infer stability yet. An interface, an
/// annotation and an enum get neither.
fn reject_stability_inferred_classes(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    rejections: &mut Vec<PluginRejection>,
) {
    for class in &ir.classes {
        if !class.is_source_declared
            || class.is_interface
            || class.is_annotation
            || class.is_enum
            || class.is_enum_entry
        {
            continue;
        }
        let start = line_start(source.text, class.decl_line);
        rejections.push(PluginRejection {
            span: crate::diag::Span::new(start, start),
            message: unsupported("a class in a Compose module, whose stability kotlinc infers"),
        });
    }
}

/// The byte offset where 1-based `line` of `text` starts.
fn line_start(text: &str, line: u32) -> u32 {
    let offset = text
        .match_indices('\n')
        .nth(line.saturating_sub(2) as usize)
        .filter(|_| line > 1)
        .map_or(0, |(index, _)| index + 1);
    offset as u32
}

/// The runtime annotations that change how kotlinc's plugin groups a composable function, or fix
/// the applier its inference would otherwise find.
const GROUP_ANNOTATIONS: [(&str, &str); 6] = [
    (
        "androidx/compose/runtime/NonRestartableComposable",
        "a `@NonRestartableComposable` function",
    ),
    (
        "androidx/compose/runtime/NonSkippableComposable",
        "a `@NonSkippableComposable` function",
    ),
    (
        "androidx/compose/runtime/ReadOnlyComposable",
        "a `@ReadOnlyComposable` function",
    ),
    (
        "androidx/compose/runtime/ExplicitGroupsComposable",
        "an `@ExplicitGroupsComposable` function",
    ),
    (
        "androidx/compose/runtime/ComposableTarget",
        "a `@ComposableTarget` function",
    ),
    (
        "androidx/compose/runtime/ComposableOpenTarget",
        "a `@ComposableOpenTarget` function",
    ),
];

/// A runtime annotation on `function` whose grouping the restart-group transform does not apply
/// yet.
fn unsupported_group_annotation(ir: &IrFile, function: FunId) -> Option<&'static str> {
    // Most of them are SOURCE-retained, so they are among the function's checked source facts
    // rather than its emitted annotations.
    let retained = ir
        .function_annotations
        .get(&function)
        .into_iter()
        .flat_map(|annotations| annotations.applications().map(|applied| applied.internal));
    let source = ir
        .function_source_annotations
        .get(&function)
        .into_iter()
        .flatten()
        .copied();
    let applied = retained.chain(source).collect::<Vec<_>>();
    GROUP_ANNOTATIONS.iter().find_map(|&(annotation, what)| {
        applied
            .contains(&crate::types::type_name(annotation))
            .then_some(what)
    })
}

/// The runtime class whose presence tells kotlinc's plugin that source information may name
/// parameters (`ComposeRuntimeFeature.SourceInfoParameterNames`).
const SOURCE_INFORMATION: &str = "androidx/compose/runtime/tooling/SourceInformation";

/// What a restartable body does with its composable lambda parameters, which their applier scheme
/// is inferred from.
fn target_constraints(
    ir: &IrFile,
    lambdas: &[bool],
    sites: &[ComposableCallSite],
) -> TargetConstraints {
    let mut invoked = Vec::new();
    let mut calls = Vec::new();
    for site in sites {
        match &ir.exprs[site.call as usize] {
            IrExpr::InvokeFunction { .. } => {
                if let [ArgumentState::Forwarded(parameter)] = site.arguments[..] {
                    invoked.push(parameter);
                }
            }
            IrExpr::Call { callee, .. } => {
                let arguments = (0..)
                    .zip(&site.arguments)
                    .filter_map(|(callee_parameter, argument)| match *argument {
                        ArgumentState::Forwarded(parameter) if lambdas[parameter as usize] => {
                            Some((callee_parameter, parameter))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !arguments.is_empty() {
                    calls.push(LambdaCall {
                        callee: callee
                            .source_function()
                            .expect("a composable call in the file calls one of its functions"),
                        arguments,
                    });
                }
            }
            _ => {}
        }
    }
    TargetConstraints {
        lambdas: (0..)
            .zip(lambdas)
            .filter(|(_, &lambda)| lambda)
            .map(|(index, _)| index)
            .collect(),
        invoked,
        calls,
    }
}

/// `sourceInformation`'s location of a composable call: its line, offset and length.
fn call_location(positions: &Utf16Offsets, span: crate::diag::Span) -> String {
    let start = positions.unit(span.lo);
    let end = positions.unit(span.hi);
    format!("{}@{start}L{}", positions.line(span.lo), end - start)
}

/// The declaration shapes the restart-group transform does not handle yet. `lambdas` says which
/// parameters are composable `() -> Unit` values, which are stable.
fn unsupported_declaration(
    declaration: &crate::ir::IrPackageFunction,
    lambdas: &[bool],
) -> Option<&'static str> {
    if declaration.receiver.is_some() || declaration.context_count != 0 {
        Some("a composable function with a receiver")
    } else if declaration.vararg_index.is_some() {
        Some("a composable function with a vararg parameter")
    } else if declaration.params.len() > SLOTS_PER_INT {
        Some("a composable function with more than ten parameters")
    } else if declaration
        .params
        .iter()
        .zip(lambdas)
        .any(|((_, ty), &lambda)| !lambda && !is_known_stable(*ty))
    {
        Some("a composable function parameter whose stability kotlinc infers")
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

/// A use of a composable `() -> Unit` parameter the transform does not handle yet: anything but
/// calling it, or passing it to a composable call, as a statement of the body.
fn unsupported_lambda_use(
    ir: &IrFile,
    block: ExprId,
    direct: &[ExprId],
    lambdas: &[bool],
) -> Option<&'static str> {
    let mut covered = crate::fx_hash::HashSet::default();
    for &statement in direct {
        match &ir.exprs[statement as usize] {
            IrExpr::InvokeFunction { func, .. } => {
                covered.insert(*func);
            }
            IrExpr::Call { args, .. } => covered.extend(args.iter().copied()),
            _ => {}
        }
    }
    let mut pending = vec![block];
    while let Some(expression) = pending.pop() {
        if let IrExpr::GetValue(index) = ir.exprs[expression as usize] {
            if lambdas.get(index as usize) == Some(&true) && !covered.contains(&expression) {
                return Some(
                    "a composable lambda parameter used other than by calling it or passing it \
                     to a composable call",
                );
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    None
}

/// Whether parameter `index` of `function` is declared `@Composable () -> Unit`, the one composable
/// function type whose calling convention the transform implements.
fn is_composable_lambda(ir: &IrFile, function: FunId, index: usize, composable: TypeName) -> bool {
    let Some(&ty) = ir.functions[function as usize].params.get(index) else {
        return false;
    };
    let TyKind::Fun(signature) = ty.kind() else {
        return false;
    };
    let Some(spelled) = ir
        .fn_declared_spellings
        .get(&function)
        .and_then(|spellings| spellings.params.get(index))
    else {
        return false;
    };
    signature.params.is_empty()
        && matches!(signature.ret.kind(), TyKind::Unit)
        && signature.context_count == 0
        && !signature.has_receiver
        && !signature.suspend
        && matches!(
            spelled.annotations.as_slice(),
            [crate::ir::TypeUseAnnotation::Checked(checked)] if checked.annotation == composable
        )
        && spelled.expansion_annotations.is_empty()
        && !spelled
            .args
            .iter()
            .any(|argument| spelled_mentions(argument, composable))
}

/// A default argument the pass does not apply yet: one that is not a constant, which kotlinc
/// evaluates in a defaults group of its own.
fn unsupported_defaults(ir: &IrFile, function: FunId) -> Option<&'static str> {
    let defaults = ir.param_defaults(function)?;
    (ir.param_defaults_stub_only(function)
        || defaults
            .iter()
            .flatten()
            .any(|&default| !matches!(ir.exprs[default as usize], IrExpr::Const(_))))
    .then_some("a composable function with a default argument that is not a constant")
}

/// The parameter types the stability inferencer knows are stable without looking further: the
/// primitives and `String`.
fn is_known_stable(ty: Ty) -> bool {
    ty == Ty::String || super::runtime::PRIMITIVES.contains(&ty)
}

/// Which of the first `count` values — the function's parameters — `block` reads.
fn used_parameters(ir: &IrFile, block: ExprId, count: u32) -> Vec<bool> {
    let mut used = vec![false; count as usize];
    let mut pending = vec![block];
    while let Some(expression) = pending.pop() {
        if let IrExpr::GetValue(index) = ir.exprs[expression as usize] {
            if index < count {
                used[index as usize] = true;
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    used
}

/// kotlinc's `argumentMetaOf` for an argument of a call in a function with `parameters` value
/// parameters, or `None` when its answer depends on more than krusty classifies yet.
fn argument_state(ir: &IrFile, argument: ExprId, parameters: u32) -> Option<ArgumentState> {
    match ir.exprs[argument as usize] {
        IrExpr::Const(_) => Some(ArgumentState::Static),
        IrExpr::GetValue(index) if index < parameters => Some(ArgumentState::Forwarded(index)),
        _ => reads_parameter(ir, argument, parameters).then_some(ArgumentState::Uncertain),
    }
}

/// Whether `expression` is an operation over a value parameter. `isStatic` asks every operand of
/// a call, an operator or a conversion to be static, and a parameter never is, so such an
/// expression is never static whatever the operation.
fn reads_parameter(ir: &IrFile, expression: ExprId, parameters: u32) -> bool {
    match &ir.exprs[expression as usize] {
        IrExpr::GetValue(index) => *index < parameters,
        IrExpr::Call { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::PrimitiveBinOp { .. }
        | IrExpr::PrimitiveNeg { .. }
        | IrExpr::Equality { .. }
        | IrExpr::StringConcat(_)
        | IrExpr::TypeOp { .. } => {
            let mut reads = false;
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                reads |= reads_parameter(ir, child, parameters);
            });
            reads
        }
        _ => false,
    }
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

/// A call whose selected declaration is composable.
struct ComposableCall {
    span: Option<crate::diag::Span>,
    /// Whether the callee is declared in this file, so the pass gives it its composer parameters.
    same_file: bool,
}

/// Every call in `ir` whose selected declaration is composable.
fn composable_calls(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    composable_functions: &crate::fx_hash::HashSet<FunId>,
    composable: TypeName,
) -> HashMap<ExprId, ComposableCall> {
    let local = |function: &FunId| composable_functions.contains(function).then_some(true);
    let module = |target: &crate::ir::CallableId| {
        if ir
            .checked_callable_functions
            .get(target)
            .is_some_and(|function| composable_functions.contains(function))
        {
            return Some(true);
        }
        ir.referenced_module_callables
            .get(target)
            .is_some_and(|callable| {
                callable
                    .annotations
                    .iter()
                    .any(|annotation| annotation.identity == composable)
            })
            .then_some(false)
    };
    ir.exprs
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            let same_file = match expression {
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
                        .checked
                        .dependency_annotations(*target)
                        .contains(&composable)
                        .then_some(false),
                    _ => None,
                },
                IrExpr::Checked(operation) => match &**operation {
                    crate::ir::IrCheckedOperation::Call { target, .. } => module(target),
                    _ => None,
                },
                _ => None,
            }?;
            let call = index as ExprId;
            Some((
                call,
                ComposableCall {
                    span: source.expression_span(ir, call),
                    same_file,
                },
            ))
        })
        .collect()
}

/// Reject every declared type that names a composable function type, except the parameters
/// `admitted` names, whose calling convention the transform implements.
fn reject_composable_types(
    ir: &IrFile,
    composable: TypeName,
    admitted: impl Fn(FunId, usize) -> bool,
    rejections: &mut Vec<PluginRejection>,
) {
    let names_composable = |spelled: &crate::ir::Spelled| spelled_mentions(spelled, composable);
    let functions = ir
        .fn_declared_spellings
        .iter()
        .filter(|(&function, spellings)| {
            names_composable(&spellings.ret)
                || names_composable(&spellings.receiver)
                || (0..)
                    .zip(&spellings.params)
                    .any(|(index, spelled)| names_composable(spelled) && !admitted(function, index))
        });
    let properties = ir
        .prop_declared_spellings
        .values()
        .filter(|spellings| {
            names_composable(&spellings.ret)
                || names_composable(&spellings.receiver)
                || spellings.params.iter().any(names_composable)
        })
        .map(|_| "a property");
    let mut names = functions
        .map(|(&function, _)| ir.functions[function as usize].name.as_str())
        .chain(properties)
        .map(str::to_string)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    rejections.extend(names.into_iter().map(|name| PluginRejection {
        span: crate::diag::Span::new(0, 0),
        message: unsupported(&format!("the composable function type in `{name}`")),
    }));
}

fn spelled_mentions(spelled: &crate::ir::Spelled, annotation: TypeName) -> bool {
    spelled
        .annotations
        .iter()
        .chain(&spelled.expansion_annotations)
        .any(|applied| matches!(applied, crate::ir::TypeUseAnnotation::Checked(checked) if checked.annotation == annotation))
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
