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
use super::class_stability::ParameterStability;
use super::durable_keys::{package_hash, radix_string, KeyPath};
use super::restart_groups::SLOTS_PER_INT;
use super::runtime::ComposeRuntime;
use super::{ComposeOptions, COMPOSABLE};

mod inline_library_calls;
use inline_library_calls::LambdaPlacement;

/// The Compose facts of one file.
pub struct ComposeFileFacts {
    pub(super) functions: Vec<RestartableFunction>,
    /// The functions that are not composable but take composable lambdas, which the transform
    /// types as the functions they are called as.
    pub(super) lambda_takers: Vec<LambdaTaker>,
    /// The file's composable lambdas, in source order.
    pub(super) lambdas: Vec<ComposableLambda>,
    /// The class holding those that capture nothing: the file's `ComposableSingletons$<File>Kt`.
    pub(super) singletons: TypeName,
    pub(super) runtime: ComposeRuntime,
}

/// A function that is not composable and takes composable function values: for each such
/// parameter, its index and the values the function value is called with.
pub(super) struct LambdaTaker {
    pub(super) function: FunId,
    pub(super) parameters: Vec<(usize, u32)>,
    /// The `@ComposableInferredTarget` scheme kotlinc infers from the functions it passes them to.
    pub(super) inferred_target: Option<String>,
}

/// A composable function the transform turns into a restartable group.
pub(super) struct RestartableFunction {
    pub(super) function: FunId,
    /// Whether the function is a restartable group. A composable function with a result and no
    /// composable call has no group: it takes the composer parameters and its trace markers only.
    pub(super) restartable: bool,
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
    /// The function's value parameters, which its `$changed` bits track slot by slot. An extension
    /// function's receiver is the first, and `$default` bits number the parameters after it.
    pub(super) parameters: Vec<TrackedParameter>,
    /// Whether the first tracked parameter is the extension receiver.
    pub(super) receiver: bool,
    /// The function's direct composable calls, in source order.
    pub(super) calls: Vec<ComposableCallSite>,
    /// The lifting sequence the function's recompose lambda is numbered in, and the function's
    /// source order among the file's declarations.
    pub(super) lifting: (crate::ir::IrLiftingSequence, u32),
}

/// A composable lambda passed to a composable call, which kotlinc's `ComposerLambdaMemoization`
/// memoizes.
pub(super) struct ComposableLambda {
    /// The lambda expression, an argument of the composable call `call`.
    pub(super) lambda: ExprId,
    pub(super) call: ExprId,
    pub(super) memoization: Memoization,
    /// The values it captures, which lead its function's parameters.
    pub(super) captures: u32,
    /// The values its `invoke` takes before the composer, the receiver first, which follow the
    /// captures; its group compares the ones its body reads.
    pub(super) parameters: Vec<TrackedParameter>,
    /// The function its body was lowered to.
    pub(super) implementation: FunId,
    /// The durable key of its group, after which its property is named.
    pub(super) key: i32,
    /// The lambda's UTF-16 start and end offsets, which `@FunctionKeyMeta` records.
    pub(super) offsets: (i32, i32),
    /// The `sourceInformation` string, when the option asks for it.
    pub(super) source_information: Option<String>,
    /// The `traceEventStart` string, when trace markers are enabled.
    pub(super) trace: Option<String>,
    /// Whether its implementation is annotated with its key.
    pub(super) key_meta: bool,
    /// The 1-based line of the opening brace, where the lambda is created and skipped.
    pub(super) opening_line: u32,
    /// Its body's direct composable calls, in source order.
    pub(super) calls: Vec<ComposableCallSite>,
    /// The form of the body an inline call places, for an [`Memoization::Inline`] lambda; its calls
    /// are `calls`.
    pub(super) inline_template: Option<ExprId>,
}

impl ComposableLambda {
    /// The types of the values its `invoke` takes before the composer.
    pub(super) fn parameter_types(&self) -> Vec<Ty> {
        self.parameters
            .iter()
            .map(|parameter| parameter.ty)
            .collect()
    }
}

/// How a composable lambda's value is kept across compositions.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Memoization {
    /// It captures nothing: one value, created once in a property of the file's
    /// `ComposableSingletons` object and read wherever it is written.
    Singleton,
    /// It captures values: `rememberComposableLambda` keeps it in the composition of `caller`,
    /// whose group creates it, and updates the block it calls.
    Remembered { caller: FunId },
    /// It is passed to an inline library composable, which places its body in the caller: no value
    /// is created, and the body runs between source-information markers in the caller's group.
    Inline { caller: FunId },
    /// It captures values in a function that is not composable, which has no composition to keep
    /// it in: `composableLambdaInstance` wraps it where `caller` passes it.
    Instance { caller: FunId },
}

/// One value parameter of a restartable function.
pub(super) struct TrackedParameter {
    pub(super) name: String,
    pub(super) ty: Ty,
    /// Whether the body reads it, which is when the function compares it to decide on skipping.
    pub(super) used: bool,
    /// Its default value, a constant the group assigns when the caller omits the argument.
    pub(super) default: Option<ExprId>,
    /// The values its `invoke` takes before the composer, the receiver counted, when it is a
    /// composable function type.
    pub(super) lambda: Option<u32>,
    /// What kotlinc infers of its type's stability, which decides how the function compares it.
    pub(super) stability: ParameterStability,
}

/// A composable call directly in a restartable body, and what the caller knows about each argument
/// it passes, which becomes the call's `$changed` bits.
pub(super) struct ComposableCallSite {
    /// A call of a composable function, or an `invoke` of a composable parameter.
    pub(super) call: ExprId,
    /// The callee's parameters in order, the omitted ones included; an `invoke`'s one slot is the
    /// called value itself.
    pub(super) arguments: Vec<ArgumentState>,
    /// A library callee declares parameter defaults, so it takes a `$default` mask after its
    /// `$changed`, whether or not the call omits an argument.
    pub(super) library_mask: bool,
    /// The first of `arguments` is the receiver of an extension function. It takes the first
    /// `$changed` slot but no `$default` bit, and is the call's dispatch receiver, not an argument.
    pub(super) receiver: bool,
    /// For each argument, the classes whose `$stable` the callee's `$changed` takes the argument's
    /// stability from, when its type's stability is decided when the caller runs.
    pub(super) stability: Vec<Vec<TypeName>>,
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
            package_functions.contains(&function)
                && composable_arity(ir, function, index, composable).is_some()
        },
        rejections,
    );
    reject_composable_property_reads(ir, source, composable, rejections);
    super::class_stability::declare(ir, source, runtime, rejections);
    if ir.file_annotations.applications().any(|annotation| {
        GROUP_ANNOTATIONS[4..]
            .iter()
            .any(|&(target, _)| annotation.internal == crate::types::type_name(target))
    }) {
        rejections.push(PluginRejection {
            refusal: true,
            span: crate::diag::Span::new(0, 0),
            message: unsupported("a file-level applier target"),
        });
    }
    let calls = composable_calls(ir, source, &composable_functions, composable);
    // A function that takes a composable lambda is retyped even in a file without a composable
    // call.
    let takes_lambdas = ir.package_functions.iter().any(|declaration| {
        (0..declaration.params.len())
            .any(|index| composable_arity(ir, declaration.function, index, composable).is_some())
    });
    if composable_functions.is_empty() && calls.is_empty() && !takes_lambdas {
        return;
    }
    let positions = Utf16Offsets::new(source.text);
    let package = ir.package.clone().unwrap_or_default().replace('/', ".");
    let file_reference = match package_hash(&package) {
        0 => source.file_name.to_string(),
        hash => format!("{}#{}", source.file_name, radix_string(hash, 36)),
    };
    let singletons = singletons_class(ir, source.file_name);
    // The class's source name, which a lambda's trace string spells.
    let singletons_name = singletons.render().replace('/', ".");
    let scope = BodyScope {
        calls: &calls,
        positions: &positions,
        composable,
        options,
        source,
        file_reference: &file_reference,
        singletons: &singletons_name,
        runtime: &*runtime,
    };
    let mut functions = Vec::new();
    let mut composable_lambdas = Vec::new();
    let mut targets = HashMap::default();
    let mut handled = crate::fx_hash::HashSet::default();
    // Every call directly in a restartable body, a rejected one included, which needs no second
    // rejection as a stray call.
    let mut direct_calls = crate::fx_hash::HashSet::default();
    // The lambdas a function that is not composable passes from its statements, which its own
    // registration or rejection answers.
    let mut answered_lambdas = crate::fx_hash::HashSet::default();
    for declaration in &ir.package_functions {
        if !composable_functions.contains(&declaration.function) {
            continue;
        }
        handled.insert(declaration.function);
        let span = declaration.signature_span;
        let lambdas = (0..declaration.params.len())
            .map(|index| composable_arity(ir, declaration.function, index, composable))
            .collect::<Vec<_>>();
        // An extension receiver is the first value of the frame, ahead of the declared parameters.
        let receiver_stability = declaration
            .receiver
            .map(|ty| super::class_stability::parameter_stability(ir, &*runtime, ty));
        let stabilities =
            receiver_stability
                .into_iter()
                .chain(declaration.params.iter().zip(&lambdas).map(
                    |((_, ty), lambda)| match lambda {
                        Some(_) => Ok(ParameterStability::Stable),
                        None => super::class_stability::parameter_stability(ir, &*runtime, *ty),
                    },
                ))
                .collect::<Vec<_>>();
        if let Some(what) = unsupported_group_annotation(ir, declaration.function)
            .or_else(|| unsupported_declaration(declaration, &stabilities))
            .or_else(|| unsupported_defaults(ir, declaration.function))
            .or_else(|| {
                (options.source_information
                    && !declaration.params.is_empty()
                    && runtime
                        .classifier(crate::types::type_name(SOURCE_INFORMATION))
                        .is_none())
                .then_some(
                    "the source information of a composable function with parameters for a \
                     runtime that predates parameter names in it",
                )
            })
        {
            rejections.push(PluginRejection {
                refusal: true,
                span,
                message: unsupported(what),
            });
            continue;
        }
        let plain = !matches!(declaration.ret.kind(), TyKind::Unit);
        let Some(body) = function_body(ir, declaration.function).or_else(|| {
            plain
                .then(|| expression_body(ir, declaration.function))
                .flatten()
        }) else {
            rejections.push(PluginRejection {
                refusal: true,
                span,
                message: unsupported("a composable function without a block body"),
            });
            continue;
        };
        let Some(body_span) = source.expression_span(ir, body.block).or_else(|| {
            // An expression body has no block of its own: its expression is the body.
            let IrExpr::Return(Some(value)) = ir.exprs[*body.statements.last()? as usize] else {
                return None;
            };
            source.expression_span(ir, value)
        }) else {
            rejections.push(PluginRejection {
                refusal: true,
                span,
                message: unsupported("a composable function whose body has no source position"),
            });
            continue;
        };
        // An extension receiver is the first value of the frame, ahead of the declared parameters.
        let lambdas = std::iter::repeat_n(None, usize::from(declaration.receiver.is_some()))
            .chain(lambdas)
            .collect::<Vec<_>>();
        let parameter_count = lambdas.len() as u32;
        if plain {
            if let Some(what) =
                unsupported_plain_body(ir, declaration.function, &body, &calls, &lambdas)
            {
                rejections.push(PluginRejection {
                    refusal: true,
                    span,
                    message: unsupported(what),
                });
                continue;
            }
        }
        let invoked = |statement: ExprId| match &ir.exprs[statement as usize] {
            IrExpr::InvokeFunction { func, .. } => match ir.exprs[*func as usize] {
                IrExpr::GetValue(index)
                    if index < parameter_count && lambdas[index as usize].is_some() =>
                {
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
            .filter_map(|statement| {
                let call = initializing_call(ir, statement, &calls);
                (calls.contains_key(&call) || invoked(call).is_some()).then_some(call)
            })
            .collect::<Vec<_>>();
        direct_calls.extend(direct.iter().copied());
        // The calls that may pass a composable lambda: the composable ones and those of functions
        // that are not composable but take one, in order, among which kotlinc counts a callee's
        // calls in its lambdas' keys.
        let passing = body
            .statements
            .iter()
            .copied()
            .filter(|&statement| {
                direct.contains(&statement)
                    || matches!(ir.exprs[statement as usize], IrExpr::Call { .. })
            })
            .collect::<Vec<_>>();
        let lambda_arguments = lambda_arguments(ir, source, &passing, composable);
        if let Some(argument) = lambda_arguments
            .iter()
            .find(|argument| argument.captures && !direct.contains(&argument.call))
        {
            rejections.push(PluginRejection {
                refusal: true,
                span: source
                    .expression_span(ir, argument.lambda)
                    .unwrap_or(crate::diag::Span::new(0, 0)),
                message: unsupported(
                    "a capturing composable lambda passed from a composable body to a function \
                     that is not composable",
                ),
            });
            include_lambda_calls(ir, &lambda_arguments, &calls, &mut direct_calls);
            continue;
        }
        if let Some(what) = (!plain)
            .then(|| unsupported_body_expression(ir, body.block, &lambda_arguments, &lambdas))
            .flatten()
        {
            rejections.push(PluginRejection {
                refusal: true,
                span,
                message: unsupported(what),
            });
            include_lambda_calls(ir, &lambda_arguments, &calls, &mut direct_calls);
            continue;
        }
        // The recompose lambda is numbered after the composable lambdas the function passes, those
        // that stay in it; a sequence that also numbers another lambda, of the function's own or of
        // an overload's, is not placed yet.
        let lifting = crate::ir::IrLiftingSequence {
            source: source.source,
            owner: "file".into(),
            container: declaration.name.as_str().into(),
        };
        let relocated = lambda_arguments
            .iter()
            .map(|argument| argument.implementation)
            .collect::<Vec<_>>();
        if ir.lifts_other_than(&lifting, &relocated) {
            rejections.push(PluginRejection {
refusal: true,
                span,
                message: unsupported(
                    "a composable function whose lambda numbering another lambda or overload shares",
                ),
            });
            include_lambda_calls(ir, &lambda_arguments, &calls, &mut direct_calls);
            continue;
        }
        if let Some(what) =
            unsupported_lambda_use(ir, source, body.block, &direct, &lambdas, composable)
        {
            rejections.push(PluginRejection {
                refusal: true,
                span,
                message: unsupported(what),
            });
            include_lambda_calls(ir, &lambda_arguments, &calls, &mut direct_calls);
            continue;
        }
        let mut sites = Vec::with_capacity(direct.len());
        let mut locations = Vec::with_capacity(direct.len());
        for &call in &direct {
            if let Some(index) = invoked(call) {
                let IrExpr::InvokeFunction { args, .. } = &ir.exprs[call as usize] else {
                    unreachable!("an invoked parameter is invoked");
                };
                // kotlinc locates an `invoke` where its selector, the parameter's name, starts: after
                // the receiver of a function type with one.
                let Some(call_span) = source.call_span(ir, call) else {
                    rejections.push(PluginRejection {
                        refusal: true,
                        span,
                        message: unsupported("a composable call without a source position"),
                    });
                    continue;
                };
                // The called value takes the slot after the arguments it is called with.
                let Some(mut arguments) = args
                    .iter()
                    .map(|&argument| {
                        argument_state(ir, argument, CallValues::Parameters(parameter_count))
                    })
                    .collect::<Option<Vec<_>>>()
                else {
                    rejections.push(PluginRejection {
                        refusal: true,
                        span: call_span,
                        message: unsupported(
                            "a composable call argument that is not a constant, a parameter, or \
                             computed from a parameter",
                        ),
                    });
                    continue;
                };
                arguments.push(ArgumentState::Forwarded(index));
                sites.push(ComposableCallSite {
                    call,
                    arguments,
                    library_mask: false,
                    receiver: false,
                    stability: Vec::new(),
                });
                locations.push(call_location(&positions, call_span));
                continue;
            }
            if let Some((site, location)) = scope.direct_call_site(
                ir,
                call,
                CallValues::Parameters(parameter_count),
                span,
                rejections,
            ) {
                // The applier scheme a library callee's parameter takes is not inferred yet, so a
                // composable lambda parameter cannot be passed on to one.
                let forwarded = site.arguments.iter().any(|argument| {
                    matches!(*argument, ArgumentState::Forwarded(parameter) if lambdas[parameter as usize].is_some())
                });
                if forwarded && matches!(calls[&call].origin, CalleeOrigin::Library(_)) {
                    rejections.push(PluginRejection {
                        refusal: true,
                        span: calls[&call].span.unwrap_or(span),
                        message: unsupported(
                            "a composable lambda parameter passed to a library composable",
                        ),
                    });
                    continue;
                }
                // A remembered lambda's `rememberComposableLambda` call is a group of its own,
                // which the function's source information lists before the call it is passed to.
                locations.extend(
                    lambda_arguments
                        .iter()
                        .filter(|argument| argument.call == call && argument.captures)
                        .filter_map(|argument| source.expression_span(ir, argument.lambda))
                        .map(|lambda| call_location(&positions, lambda)),
                );
                sites.push(site);
                locations.push(location);
            }
        }
        let FunctionNames {
            name,
            qualified,
            signature,
        } = FunctionNames::of(
            ir,
            declaration,
            &package,
            &lambdas[usize::from(declaration.receiver.is_some())..],
        );
        if lambdas.iter().any(Option::is_some) {
            targets.insert(
                declaration.function,
                target_constraints(ir, &lambdas, &sites),
            );
        }
        // A function without a group decides nothing on its parameters.
        let used = if plain {
            vec![false; parameter_count as usize]
        } else {
            used_parameters(ir, body.block, 0, parameter_count)
        };
        let defaults = ir
            .param_defaults(declaration.function)
            .cloned()
            .unwrap_or_default();
        // The recompose lambda names its captured receiver `this_<function>`.
        let receiver = declaration.receiver.map(|ty| (format!("this_{name}"), ty));
        let parameters = receiver
            .iter()
            .chain(&declaration.params)
            .zip(used)
            .enumerate()
            .map(|(frame, ((name, ty), used))| TrackedParameter {
                name: name.clone(),
                ty: *ty,
                used,
                default: defaults.get(frame).copied().flatten(),
                lambda: lambdas[frame],
                stability: stabilities[frame].unwrap_or(ParameterStability::Undecided),
            })
            .collect::<Vec<_>>();
        // `parameterNameInformation`: the source parameters' names, which the runtime's tooling
        // maps the tracked slots back to.
        let declared = &parameters[usize::from(declaration.receiver.is_some())..];
        let names = if declared.is_empty() {
            String::new()
        } else {
            format!(
                "N({})",
                declared
                    .iter()
                    .map(|parameter| parameter.name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let path = KeyPath {
            signature: &signature,
            package: &package,
            file_name: source.file_name,
            within: None,
        };
        scope.register_lambdas(
            ir,
            LambdaOwner {
                path: &path,
                qualified: &qualified,
                caller: declaration.function,
                caller_parameters: parameter_count,
                direct: &passing,
                arguments: &lambda_arguments,
                composable: true,
            },
            &mut direct_calls,
            rejections,
            &mut composable_lambdas,
        );
        functions.push(RestartableFunction {
            function: declaration.function,
            restartable: !plain,
            key: path.key(),
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
            receiver: declaration.receiver.is_some(),
            calls: sites,
            lifting: (lifting, declaration.source_order),
        });
    }
    // A function that is not composable has no group, but a composable lambda it passes to a
    // function (an activity's `setContent { Screen() }`) is one the pass places all the same.
    let mut lambda_takers = Vec::new();
    for declaration in &ir.package_functions {
        if composable_functions.contains(&declaration.function) {
            continue;
        }
        let arities = (0..declaration.params.len())
            .map(|index| composable_arity(ir, declaration.function, index, composable))
            .collect::<Vec<_>>();
        let direct = function_body(ir, declaration.function)
            .map(|body| body.statements)
            .unwrap_or_default()
            .into_iter()
            .filter(|&statement| matches!(ir.exprs[statement as usize], IrExpr::Call { .. }))
            .collect::<Vec<_>>();
        let lambda_arguments = lambda_arguments(ir, source, &direct, composable);
        answered_lambdas.extend(lambda_arguments.iter().map(|argument| argument.lambda));
        // The transform retypes the function's composable parameters, so it covers only the uses
        // that see the composer-taking function: passing one to a composable lambda parameter.
        let frame = std::iter::repeat_n(None, usize::from(declaration.receiver.is_some()))
            .chain(arities.iter().copied())
            .collect::<Vec<_>>();
        if let Some(what) = ir.functions[declaration.function as usize]
            .body
            .and_then(|root| unsupported_lambda_use(ir, source, root, &direct, &frame, composable))
        {
            rejections.push(PluginRejection {
                refusal: true,
                span: declaration.signature_span,
                message: unsupported(what),
            });
            include_lambda_calls(ir, &lambda_arguments, &calls, &mut direct_calls);
            continue;
        }
        // The applier a parameter needs is the one of the function it is passed to, which the
        // inference knows for a function of the file only.
        let mut forwards = Vec::new();
        let mut foreign = None;
        for &call in &direct {
            let IrExpr::Call { callee, args, .. } = &ir.exprs[call as usize] else {
                continue;
            };
            let arguments = supplied_arguments(callee, args)
                .filter_map(|(parameter, argument)| match ir.exprs[argument as usize] {
                    IrExpr::GetValue(index)
                        if frame.get(index as usize).is_some_and(Option::is_some) =>
                    {
                        Some((parameter, index))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if arguments.is_empty() {
                continue;
            }
            match callee.source_function() {
                Some(function) => {
                    // The callee's frame starts with its extension receiver.
                    let receiver = u32::from(ir.extension_receiver_fns.contains(&function));
                    forwards.push(LambdaCall {
                        callee: function,
                        arguments: arguments
                            .into_iter()
                            .map(|(parameter, index)| (parameter + receiver, index))
                            .collect(),
                    });
                }
                None => foreign = Some(call),
            }
        }
        if let Some(call) = foreign {
            rejections.push(PluginRejection {
                refusal: true,
                span: source
                    .expression_span(ir, call)
                    .unwrap_or(declaration.signature_span),
                message: unsupported(
                    "a composable lambda parameter of a function that is not composable passed to \
                     a function of another file or a library",
                ),
            });
            include_lambda_calls(ir, &lambda_arguments, &calls, &mut direct_calls);
            continue;
        }
        let parameters = arities
            .iter()
            .enumerate()
            .filter_map(|(index, arity)| arity.map(|arity| (index, arity)))
            .collect::<Vec<_>>();
        if !parameters.is_empty() {
            targets.insert(
                declaration.function,
                TargetConstraints {
                    lambdas: (0..)
                        .zip(&frame)
                        .filter(|(_, lambda)| lambda.is_some())
                        .map(|(index, _)| index)
                        .collect(),
                    invoked: Vec::new(),
                    calls: forwards,
                },
            );
            lambda_takers.push(LambdaTaker {
                function: declaration.function,
                parameters,
                inferred_target: None,
            });
        }
        if lambda_arguments.is_empty() {
            continue;
        }
        let FunctionNames {
            qualified,
            signature,
            ..
        } = FunctionNames::of(ir, declaration, &package, &arities);
        let path = KeyPath {
            signature: &signature,
            package: &package,
            file_name: source.file_name,
            within: None,
        };
        scope.register_lambdas(
            ir,
            LambdaOwner {
                path: &path,
                qualified: &qualified,
                caller: declaration.function,
                caller_parameters: ir.functions[declaration.function as usize].params.len() as u32,
                direct: &direct,
                arguments: &lambda_arguments,
                composable: false,
            },
            &mut direct_calls,
            rejections,
            &mut composable_lambdas,
        );
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
            for taker in &mut lambda_takers {
                taker.inferred_target = schemes
                    .get(&taker.function)
                    .and_then(applier_targets::Scheme::inferred);
            }
        }
        Err(recursive) => rejections.push(PluginRejection {
            refusal: true,
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
                refusal: true,
                span: crate::diag::Span::new(0, 0),
                message: unsupported(&format!(
                    "the composable member or local function `{}`",
                    ir.functions[function as usize].name
                )),
            });
        }
    }
    // Outside a composable body, the pass places only a lambda a function's statement passes; any
    // other is passed as it is to a parameter the transform retyped to take a composer.
    let composable_bodies = composable_functions
        .iter()
        .filter_map(|&function| ir.functions[function as usize].body)
        .chain(composable_lambdas.iter().map(|lambda| lambda.lambda));
    let mut refused = None;
    for (lambda, span) in
        unplaced_lambdas(ir, source, composable, composable_bodies, &answered_lambdas)
    {
        include_calls_of(ir, lambda, &calls, &mut direct_calls);
        // A lambda's body is lowered twice, once for its inline form, so a lambda in it is met
        // twice.
        if refused.replace(span) == Some(span) {
            continue;
        }
        rejections.push(PluginRejection {
            refusal: true,
            span,
            message: unsupported(
                "a composable lambda passed other than by a statement of a function body",
            ),
        });
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
        refusal: true,
        span,
        message: unsupported("a composable call outside the statements of a composable body"),
    }));
    // The function whose parameters a site's forwarded arguments are: a restartable function, or
    // the implementation of a composable lambda.
    let sites = functions
        .iter_mut()
        .flat_map(|function| {
            let caller = function.function;
            function.calls.iter_mut().map(move |site| (caller, site))
        })
        .chain(composable_lambdas.iter_mut().flat_map(|lambda| {
            let caller = lambda.implementation;
            lambda.calls.iter_mut().map(move |site| (caller, site))
        }));
    for (caller, site) in sites {
        match argument_stability(ir, &*runtime, site, caller) {
            Ok(stability) => site.stability = stability,
            Err(what) => rejections.push(PluginRejection {
                refusal: true,
                span: calls
                    .get(&site.call)
                    .and_then(|call| call.span)
                    .unwrap_or(crate::diag::Span::new(0, 0)),
                message: unsupported(what),
            }),
        }
    }
    if functions.is_empty() && composable_lambdas.is_empty() && lambda_takers.is_empty() {
        return;
    }
    // A library composable was compiled with the composer parameters the pass passes it.
    let sites = functions
        .iter()
        .flat_map(|function| &function.calls)
        .chain(composable_lambdas.iter().flat_map(|lambda| &lambda.calls));
    for site in sites {
        if let Some(CalleeOrigin::Library(target)) = calls.get(&site.call).map(|call| call.origin) {
            let mut appended = vec![Ty::nullable(super::runtime::composer())];
            appended.extend(
                (0..super::restart_groups::changed_ints(site.arguments.len())).map(|_| Ty::Int),
            );
            // The `$default` mask of a declaration with defaults follows its `$changed` ints.
            if site.library_mask {
                appended.push(Ty::Int);
            }
            runtime.appended_parameters(target, appended);
        }
    }
    match ComposeRuntime::bind(runtime) {
        Ok(runtime) => {
            // kotlinc memoizes a lambda once its body is transformed, so one nested in another
            // comes first: by where each ends.
            composable_lambdas.sort_by_key(|lambda| (lambda.offsets.1, lambda.offsets.0));
            ir.compose = Some(Box::new(ComposeFileFacts {
                functions,
                lambda_takers,
                lambdas: composable_lambdas,
                singletons,
                runtime,
            }));
        }
        Err(missing) => {
            let span = functions
                .first()
                .map(|first| first.function)
                .or_else(|| lambda_takers.first().map(|taker| taker.function))
                .and_then(|function| {
                    ir.package_functions
                        .iter()
                        .find(|declaration| declaration.function == function)
                })
                .map_or(crate::diag::Span::new(0, 0), |declaration| {
                    declaration.signature_span
                });
            rejections.push(PluginRejection {
                refusal: true,
                span,
                message: format!(
                    "krusty's Compose compiler plugin cannot compile this module: {missing}"
                ),
            });
        }
    }
}

/// A decline of `class`, reported at the start of its declaration's line.
pub(super) fn class_rejection(
    source: &PluginSourceView<'_>,
    class: &crate::ir::IrClass,
    what: &str,
) -> PluginRejection {
    let start = crate::plugins::line_start(source.text, class.decl_line);
    PluginRejection {
        refusal: true,
        span: crate::diag::Span::new(start, start),
        message: unsupported(what),
    }
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
    lambdas: &[Option<u32>],
    sites: &[ComposableCallSite],
) -> TargetConstraints {
    let mut invoked = Vec::new();
    let mut calls = Vec::new();
    for site in sites {
        match &ir.exprs[site.call as usize] {
            IrExpr::InvokeFunction { .. } => {
                if let Some(&ArgumentState::Forwarded(parameter)) = site.arguments.last() {
                    invoked.push(parameter);
                }
            }
            IrExpr::Call { callee, .. } => {
                let arguments = (0..)
                    .zip(&site.arguments)
                    .filter_map(|(callee_parameter, argument)| match *argument {
                        ArgumentState::Forwarded(parameter)
                            if lambdas[parameter as usize].is_some() =>
                        {
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
            .filter(|(_, lambda)| lambda.is_some())
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

/// The declaration shapes the restart-group transform does not handle yet. `stabilities` is what
/// kotlinc infers of each tracked value's type, the receiver's first, or why it is not decided.
fn unsupported_declaration(
    declaration: &crate::ir::IrPackageFunction,
    stabilities: &[Result<ParameterStability, &'static str>],
) -> Option<&'static str> {
    if declaration.context_count != 0 {
        Some("a composable function with a context parameter")
    } else if declaration.vararg_index.is_some() {
        Some("a composable function with a vararg parameter")
    } else if let Some(&Err(what)) = stabilities.iter().find(|stability| stability.is_err()) {
        Some(what)
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

/// What a composable function with a result does that the pass does not cover yet. The function
/// has no group, so its body makes no composable call and writes no lambda, takes no default
/// argument, and its one `return` ends the body.
fn unsupported_plain_body(
    ir: &IrFile,
    function: FunId,
    body: &FunctionBody,
    calls: &HashMap<ExprId, ComposableCall>,
    lambdas: &[Option<u32>],
) -> Option<&'static str> {
    if lambdas.iter().any(Option::is_some) {
        return Some("a composable function with a result and a composable lambda parameter");
    }
    if ir
        .param_defaults(function)
        .is_some_and(|defaults| defaults.iter().any(Option::is_some))
    {
        return Some("a composable function with a result and a default argument");
    }
    let Some((&last, leading)) = body.statements.split_last() else {
        return Some("a composable function with a result and no `return`");
    };
    if !matches!(ir.exprs[last as usize], IrExpr::Return(Some(_))) {
        return Some("a composable function with a result that does not end in a `return`");
    }
    let mut pending = leading.to_vec();
    pending.extend(match &ir.exprs[last as usize] {
        IrExpr::Return(value) => *value,
        _ => None,
    });
    while let Some(expression) = pending.pop() {
        if calls.contains_key(&expression) {
            return Some("a composable function with a result that makes a composable call");
        }
        match &ir.exprs[expression as usize] {
            IrExpr::Return(_) => {
                return Some("a composable function with a result and an early `return`")
            }
            IrExpr::Lambda { .. } => {
                return Some("a composable function with a result that writes a lambda")
            }
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    None
}

/// An expression in a composable body the transform does not handle yet: an early `return`, which
/// must end the group first, a lambda other than the composable `passed` to its calls, whose
/// lifted name the group's own lambda would share, or one of those that captures anything but a
/// value parameter. `lambdas` says which of the body's parameters are
/// composable `() -> Unit` values, which a lambda calling them would have to pass its composer.
fn unsupported_body_expression(
    ir: &IrFile,
    block: ExprId,
    passed: &[LambdaArgument],
    lambdas: &[Option<u32>],
) -> Option<&'static str> {
    let mut pending = vec![block];
    while let Some(expression) = pending.pop() {
        match &ir.exprs[expression as usize] {
            IrExpr::Return(_) => return Some("a `return` in a composable function"),
            IrExpr::Lambda { captures, .. }
                if passed.iter().any(|argument| argument.lambda == expression) =>
            {
                for &capture in captures {
                    match ir.exprs[capture as usize] {
                        IrExpr::GetValue(index)
                            if lambdas.get(index as usize).is_some_and(Option::is_some) =>
                        {
                            return Some(
                                "a composable lambda that captures a composable lambda parameter",
                            );
                        }
                        IrExpr::GetValue(_) => {}
                        _ => return Some("a composable lambda that captures anything but a value"),
                    }
                }
            }
            IrExpr::Lambda { .. } => return Some("a lambda in a composable function"),
            _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
    }
    None
}

/// The source spans of the lambdas passed to composable lambda parameters outside `composable_bodies`
/// (the bodies of composable functions and lambdas, whose own checks answer every lambda in them)
/// other than the `answered` ones, in source order, with each of the lambda's lowerings.
fn unplaced_lambdas(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    composable: TypeName,
    composable_bodies: impl Iterator<Item = ExprId>,
    answered: &crate::fx_hash::HashSet<ExprId>,
) -> Vec<(ExprId, crate::diag::Span)> {
    let mut inside = crate::fx_hash::HashSet::default();
    let mut pending = composable_bodies.collect::<Vec<_>>();
    while let Some(expression) = pending.pop() {
        if !inside.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.exprs[expression as usize] {
            pending.extend(ir.functions[impl_fn as usize].body);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    let answered_spans = answered
        .iter()
        .filter_map(|&lambda| source.expression_span(ir, lambda))
        .collect::<crate::fx_hash::HashSet<_>>();
    let mut spans = Vec::new();
    for expression in &ir.exprs {
        let IrExpr::Call { callee, args, .. } = expression else {
            continue;
        };
        for (parameter, argument) in supplied_arguments(callee, args) {
            if matches!(ir.exprs[argument as usize], IrExpr::Lambda { .. })
                && !inside.contains(&argument)
                && !answered.contains(&argument)
                && composable_lambda_parameter(ir, source, callee, parameter, composable)
            {
                if let Some(span) = source
                    .expression_span(ir, argument)
                    .filter(|span| !answered_spans.contains(span))
                {
                    spans.push((argument, span));
                }
            }
        }
    }
    spans.sort_by_key(|(_, span)| (span.lo, span.hi));
    spans
}

/// Count the composable calls in the bodies of `arguments`' lambdas and the lambdas in them, both
/// lowerings of each, as answered by the rejection of the function that passes them.
fn include_lambda_calls(
    ir: &IrFile,
    arguments: &[LambdaArgument],
    calls: &HashMap<ExprId, ComposableCall>,
    direct_calls: &mut crate::fx_hash::HashSet<ExprId>,
) {
    for argument in arguments {
        include_calls_of(ir, argument.lambda, calls, direct_calls);
    }
}

/// Count the composable calls in both lowerings of `lambda`'s body and the lambdas in it, as
/// answered by `lambda`'s own rejection.
fn include_calls_of(
    ir: &IrFile,
    lambda: ExprId,
    calls: &HashMap<ExprId, ComposableCall>,
    direct_calls: &mut crate::fx_hash::HashSet<ExprId>,
) {
    let mut pending = vec![lambda];
    while let Some(expression) = pending.pop() {
        if calls.contains_key(&expression) {
            direct_calls.insert(expression);
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.exprs[expression as usize] {
            pending.extend(ir.functions[impl_fn as usize].body);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

/// A lambda passed to a composable lambda parameter of a composable call.
pub(super) struct LambdaArgument {
    call: ExprId,
    lambda: ExprId,
    implementation: FunId,
    /// Whether it captures values.
    captures: bool,
    /// The callee's parameter it is passed to.
    parameter: u32,
    /// The anonymous function's `signatureString`, which its durable key names it by.
    signature: String,
    /// Whether its first parameter is the receiver of its function type.
    receiver: bool,
}

/// The lambdas `direct`'s calls pass to composable lambda parameters of a function of the file or of
/// a library.
fn lambda_arguments(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    direct: &[ExprId],
    composable: TypeName,
) -> Vec<LambdaArgument> {
    let mut arguments = Vec::new();
    for &call in direct {
        let IrExpr::Call { callee, args, .. } = &ir.exprs[call as usize] else {
            continue;
        };
        for (parameter, argument) in supplied_arguments(callee, args) {
            if let IrExpr::Lambda {
                impl_fn, captures, ..
            } = &ir.exprs[argument as usize]
            {
                if composable_lambda_parameter(ir, source, callee, parameter, composable) {
                    arguments.push(LambdaArgument {
                        call,
                        lambda: argument,
                        implementation: *impl_fn,
                        captures: !captures.is_empty(),
                        parameter,
                        signature: anonymous_signature(ir, callee, parameter),
                        receiver: has_receiver(ir, callee, parameter),
                    });
                }
            }
        }
    }
    arguments
}

/// The function type parameter `parameter` of `callee` declares.
fn declared_function_type(ir: &IrFile, callee: &Callee, parameter: u32) -> Option<Ty> {
    match callee {
        Callee::External { params, .. } => params.get(parameter as usize).copied(),
        _ => callee
            .source_function()
            .and_then(|function| {
                ir.functions[function as usize]
                    .params
                    .get(parameter as usize)
            })
            .copied(),
    }
}

/// Whether the function type parameter `parameter` of `callee` declares has a receiver.
fn has_receiver(ir: &IrFile, callee: &Callee, parameter: u32) -> bool {
    declared_function_type(ir, callee, parameter)
        .is_some_and(|ty| matches!(ty.kind(), TyKind::Fun(signature) if signature.has_receiver))
}

/// `IrSimpleFunction.signatureString()` of the anonymous function a lambda passed to parameter
/// `parameter` of `callee` lowers to: `[Receiver.]$anonymous$(Param,…)Unit`.
fn anonymous_signature(ir: &IrFile, callee: &Callee, parameter: u32) -> String {
    let declared = declared_function_type(ir, callee, parameter);
    let (receiver, parameters) = match declared.as_ref().map(|ty| ty.kind()) {
        Some(TyKind::Fun(signature)) => {
            let (receiver, rest) = match signature.params.split_first() {
                Some((first, rest)) if signature.has_receiver => (Some(*first), rest),
                _ => (None, signature.params.as_slice()),
            };
            (receiver, rest.to_vec())
        }
        _ => (None, Vec::new()),
    };
    format!(
        "{}$anonymous$({})Unit",
        receiver.map_or(String::new(), |receiver| format!(
            "{}.",
            simple_type_name(receiver)
        )),
        parameters
            .into_iter()
            .map(simple_type_name)
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Whether parameter `parameter` of the function `callee` selects is a `@Composable () -> Unit`, in
/// the file or in a library the Compose compiler plugin compiled.
fn composable_lambda_parameter(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    callee: &Callee,
    parameter: u32,
    composable: TypeName,
) -> bool {
    if let Some(function) = callee.source_function() {
        return composable_arity(ir, function, parameter as usize, composable).is_some();
    }
    let Callee::External { target, params, .. } = callee else {
        return false;
    };
    let Some(shape) = source.checked.dependency_shape(*target) else {
        return false;
    };
    shape.composable_parameters.get(parameter as usize) == Some(&true)
        && params.get(parameter as usize).is_some_and(|ty| {
            matches!(
                ty.kind(),
                TyKind::Fun(signature)
                    if matches!(signature.ret.kind(), TyKind::Unit)
                        && signature.params.len() < SLOTS_PER_INT
                        && signature.context_count == 0
                        && !signature.suspend
            )
        })
}

/// Each argument a call supplies, with the callee parameter it is passed to; the parameters it
/// omits take their defaults.
fn supplied_arguments<'a>(
    callee: &'a Callee,
    args: &'a [ExprId],
) -> impl Iterator<Item = (u32, ExprId)> + 'a {
    let omitted = omitted_parameters(callee);
    (0u32..)
        .filter(move |parameter| !omitted.contains(parameter))
        .zip(args.iter().copied())
}

/// The parameters whose arguments a call omits.
fn omitted_parameters(callee: &Callee) -> &[u32] {
    match callee {
        Callee::LocalWithDefaults { defaults, .. } => defaults,
        Callee::External { defaults, .. } => defaults,
        _ => &[],
    }
}

/// Whether `synthetic`, the parameters a library composable's entry takes after its `parameters`
/// source parameters, are what `ComposerParamTransformer` appends: the composer, a `$changed` word
/// for every ten parameters and, for a declaration with defaults, a `$default` word. The pass
/// passes one `$default` word, which covers at most 31 parameters.
pub(super) fn takes_composer_frame(synthetic: &[Ty], parameters: usize, defaults: bool) -> bool {
    let default_words = match (defaults, parameters <= 31) {
        (false, _) => 0,
        (true, true) => 1,
        (true, false) => return false,
    };
    let words = super::restart_groups::changed_ints(parameters) as usize + default_words;
    matches!(synthetic, [composer, rest @ ..]
        if composer.obj_internal().is_some_and(|name| name.matches(super::runtime::COMPOSER))
            && rest.len() == words
            && rest.iter().all(|word| *word == Ty::Int))
}

/// What each argument of `site` tells its callee of its stability: the classes whose `$stable` the
/// call reads, or why the call cannot say yet.
fn argument_stability(
    ir: &IrFile,
    runtime: &dyn PluginRuntime,
    site: &ComposableCallSite,
    caller: FunId,
) -> Result<Vec<Vec<TypeName>>, &'static str> {
    let none = vec![Vec::new(); site.arguments.len()];
    let IrExpr::Call { callee, .. } = &ir.exprs[site.call as usize] else {
        return Ok(none);
    };
    let params = match &**callee {
        Callee::External { params, .. } => params.to_vec(),
        Callee::Local(function) => ir.functions[*function as usize].params.clone(),
        _ => return Ok(none),
    };
    // A library extension's receiver is its dispatch receiver, which the callee's parameters do
    // not include; its type is not known here, so only the caller's own parameter or a
    // constant says what it is.
    let receivers = usize::from(site.receiver && matches!(&**callee, Callee::External { .. }));
    let (receiver, declared) = site.arguments.split_at(receivers);
    receiver
        .iter()
        .map(|argument| match argument {
            ArgumentState::Static | ArgumentState::Omitted => Ok(Vec::new()),
            ArgumentState::Forwarded(parameter) => {
                forwarded_stability(ir, runtime, caller, *parameter)
            }
            ArgumentState::Uncertain => {
                Err("a call argument whose type's stability is decided when it runs")
            }
        })
        .chain(
            declared
                .iter()
                .zip(&params)
                .map(|(argument, &ty)| match argument {
                    ArgumentState::Static | ArgumentState::Omitted => Ok(Vec::new()),
                    ArgumentState::Forwarded(parameter) => {
                        forwarded_stability(ir, runtime, caller, *parameter)
                    }
                    ArgumentState::Uncertain => {
                        // The argument's own type may be narrower than the formal one, so only a formal
                        // type whose stability is certain says what the argument's is.
                        match super::class_stability::parameter_stability(ir, runtime, ty)? {
                            ParameterStability::Undecided => Err(
                                "a call argument whose type's stability is decided when it runs",
                            ),
                            ParameterStability::Stable | ParameterStability::Unstable => {
                                Ok(Vec::new())
                            }
                        }
                    }
                }),
        )
        .collect()
}

/// The classes whose `$stable` a call reads for an argument that forwards `parameter` of `caller`:
/// the stability of what is passed is that of the caller's own parameter, which may be a subtype of
/// the callee's formal type.
fn forwarded_stability(
    ir: &IrFile,
    runtime: &dyn PluginRuntime,
    caller: FunId,
    parameter: u32,
) -> Result<Vec<TypeName>, &'static str> {
    super::class_stability::parameter_stability_reads(
        ir,
        runtime,
        ir.functions[caller as usize].params[parameter as usize],
    )
}

/// What a call to a composable function of another file of the module does that the pass does not
/// cover yet. The callee's own file gives it its composer parameters, so the call is covered
/// where the callee's declaration shape is one that pass transforms and the call selects it
/// directly.
fn unsupported_module_call(ir: &IrFile, call: ExprId) -> Option<&'static str> {
    let IrExpr::Call {
        callee,
        dispatch_receiver,
        ..
    } = &ir.exprs[call as usize]
    else {
        unreachable!("a composable call is a call");
    };
    let (Callee::Module { target, .. }
    | Callee::CrossFile {
        module_target: Some(target),
        module_default_call: false,
        ..
    }) = &**callee
    else {
        return Some(match &**callee {
            Callee::ModuleWithDefaults { .. }
            | Callee::ModuleDefaultEntry { .. }
            | Callee::CrossFile {
                module_default_call: true,
                ..
            } => "a call to a composable function of another file with default arguments",
            _ => "a call to a composable member of another file",
        });
    };
    let Some(callable) = ir.referenced_module_callables.get(target) else {
        return Some("a call to a composable function of another file the module does not declare");
    };
    if dispatch_receiver.is_some() || callable.owner.is_some() {
        Some("a call to a composable member of another file")
    } else if callable
        .parameter_identities
        .iter()
        .any(|identity| identity.role != crate::ir::IrParameterRole::Value)
    {
        Some("a call to a composable function of another file with a receiver")
    } else if callable.parameters.len() > super::restart_groups::SLOTS_PER_INT {
        Some("a call to a composable function of another file with more than ten parameters")
    } else if !callable.default_parameters.is_empty() {
        Some("a call to a composable function of another file with default arguments")
    } else if !callable.type_parameters.is_empty() {
        Some("a call to a generic composable function of another file")
    } else if callable.is_inline() {
        Some("a call to an inline composable function of another file")
    } else if callable.is_suspend() {
        Some("a call to a suspend composable function of another file")
    } else if !matches!(callable.result.kind(), TyKind::Unit) {
        Some("a call to a composable function of another file with a result")
    } else if callable.parameters.iter().any(|&ty| !is_known_stable(ty)) {
        Some("a call to a composable function of another file with a parameter whose stability kotlinc infers")
    } else {
        None
    }
}

/// The parameter types the stability inferencer knows are stable without looking further: the
/// primitives and `String`.
fn is_known_stable(ty: Ty) -> bool {
    ty == Ty::String || super::runtime::PRIMITIVES.contains(&ty)
}

/// The Kotlin name of the function a call calls, of the file or of a library.
fn callee_name<'a>(
    ir: &'a IrFile,
    source: &'a PluginSourceView<'_>,
    call: ExprId,
) -> Option<&'a str> {
    let IrExpr::Call { callee, .. } = &ir.exprs[call as usize] else {
        return None;
    };
    if let Callee::External { target, .. } = &**callee {
        return source
            .checked
            .dependency_callable(*target)
            .map(|declaration| declaration.name);
    }
    let function = callee.source_function()?;
    Some(
        ir.fn_source_names
            .get(&function)
            .unwrap_or(&ir.functions[function as usize].name),
    )
}

/// The file's `ComposableSingletons` class, named after the file as kotlinc names the file's own
/// class (`PackagePartClassUtils.getFilePartShortName`), whatever `@JvmName` the file has.
fn singletons_class(ir: &IrFile, file_name: &str) -> TypeName {
    let stem = file_name.strip_suffix(".kt").unwrap_or(file_name);
    let name = format!(
        "ComposableSingletons${}",
        crate::names::file_part_short_name(stem)
    );
    match ir.package.as_deref() {
        Some(package) if !package.is_empty() => {
            crate::types::type_name(&format!("{package}/{name}"))
        }
        _ => crate::types::type_name(&name),
    }
}

/// The source block common lowering gives a lambda's function: its body is a block of the
/// source `{ … }`, wrapped once more, and the `return Unit` that ends it.
pub(super) fn lambda_body(ir: &IrFile, function: FunId) -> Option<FunctionBody> {
    let root = ir.functions[function as usize].body?;
    let IrExpr::Block { stmts, value: None } = &ir.exprs[root as usize] else {
        return None;
    };
    let [wrapper, end] = stmts.as_slice() else {
        return None;
    };
    if !matches!(ir.exprs[*end as usize], IrExpr::Return(Some(_))) {
        return None;
    }
    let IrExpr::Block {
        stmts: wrapped,
        value: None,
    } = &ir.exprs[*wrapper as usize]
    else {
        return None;
    };
    let [block] = wrapped.as_slice() else {
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

/// What a file's composable bodies are classified against.
/// What names a composable lambda: its group key, how it is memoized, and the source name of the
/// function that writes it.
struct LambdaIdentity<'a> {
    key: i32,
    memoization: Memoization,
    qualified_caller: &'a str,
    /// The values of the lambda's caller that are its parameters.
    caller_parameters: u32,
    /// The path of the scope that passes the lambda, its callee and the number of that callee's
    /// calls before the lambda's, which key what the lambda's body passes in turn.
    path: &'a KeyPath<'a>,
    callee: Option<&'a str>,
    sibling: u32,
}

/// How kotlinc's durable keys and traces name a package function: its source name, that name
/// qualified with the package, and its `signatureString`, which spells a composable function-type
/// parameter as the frontend's type for it, `ComposableFunction<arity>`, whether the function is
/// composable or not.
struct FunctionNames {
    name: String,
    qualified: String,
    signature: String,
}

impl FunctionNames {
    /// The names of `declaration`, whose declared parameters' composable arities are `lambdas`.
    fn of(
        ir: &IrFile,
        declaration: &crate::ir::IrPackageFunction,
        package: &str,
        lambdas: &[Option<u32>],
    ) -> Self {
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
        let signature = format!(
            "{}{name}({}){}",
            declaration
                .receiver
                .map(|receiver| format!("{}.", simple_type_name(receiver)))
                .unwrap_or_default(),
            declaration
                .params
                .iter()
                .zip(lambdas)
                .map(|((_, ty), lambda)| match lambda {
                    Some(arity) => format!("ComposableFunction{arity}"),
                    None => simple_type_name(*ty),
                })
                .collect::<Vec<_>>()
                .join(","),
            simple_type_name(declaration.ret)
        );
        FunctionNames {
            name,
            qualified,
            signature,
        }
    }
}

/// The function whose body passes composable lambdas: its durable key path, qualified name and id.
struct LambdaOwner<'a> {
    path: &'a KeyPath<'a>,
    qualified: &'a str,
    caller: FunId,
    /// The values of the caller that are its parameters.
    caller_parameters: u32,
    /// The composable calls directly in its body, in order.
    direct: &'a [ExprId],
    /// The lambdas those calls pass to composable lambda parameters.
    arguments: &'a [LambdaArgument],
    /// Whether the body is composable, which gives a capturing lambda a composition to be kept in.
    composable: bool,
}

struct BodyScope<'a> {
    calls: &'a HashMap<ExprId, ComposableCall>,
    positions: &'a Utf16Offsets,
    composable: TypeName,
    options: &'a ComposeOptions,
    source: &'a PluginSourceView<'a>,
    /// The file as source information names it, with its package hash.
    file_reference: &'a str,
    /// The source name of the file's `ComposableSingletons` class.
    singletons: &'a str,
    /// The runtime, which decides the stability of the types a lambda is called with.
    runtime: &'a dyn PluginRuntime,
}

impl BodyScope<'_> {
    /// The composable lambdas `lambda_arguments` of the `direct` calls of `owner`'s body are, each
    /// keyed by its position among its caller's lambdas.
    fn register_lambdas(
        &self,
        ir: &IrFile,
        owner: LambdaOwner<'_>,
        direct_calls: &mut crate::fx_hash::HashSet<ExprId>,
        rejections: &mut Vec<PluginRejection>,
        lambdas: &mut Vec<ComposableLambda>,
    ) {
        for argument in owner.arguments {
            let callee = callee_name(ir, self.source, argument.call);
            let sibling = owner
                .direct
                .iter()
                .take_while(|&&call| call != argument.call)
                .filter(|&&call| callee.is_some() && callee_name(ir, self.source, call) == callee)
                .count() as u32;
            let key = owner.path.lambda_key(
                &argument.signature,
                callee.unwrap_or_default(),
                sibling,
                argument.parameter,
            );
            let memoization = match (
                self.places_lambda(ir, argument.call, argument.lambda),
                argument.captures,
                owner.composable,
            ) {
                (true, _, _) => Memoization::Inline {
                    caller: owner.caller,
                },
                (false, false, _) => Memoization::Singleton,
                (false, true, true) => Memoization::Remembered {
                    caller: owner.caller,
                },
                (false, true, false) => Memoization::Instance {
                    caller: owner.caller,
                },
            };
            if let Some(lambda) = self.composable_lambda(
                ir,
                argument,
                LambdaIdentity {
                    key,
                    memoization,
                    qualified_caller: owner.qualified,
                    caller_parameters: owner.caller_parameters,
                    path: owner.path,
                    callee,
                    sibling,
                },
                direct_calls,
                rejections,
                lambdas,
            ) {
                lambdas.push(lambda);
            }
        }
    }

    /// The site of `call`, a direct call of one of the file's composable functions in a body
    /// whose first `values` values are its parameters, when `forwards`, or a lambda's captures,
    /// and its `sourceInformation` location; `None` after rejecting it.
    fn direct_call_site(
        &self,
        ir: &IrFile,
        call: ExprId,
        values: CallValues<'_>,
        span: crate::diag::Span,
        rejections: &mut Vec<PluginRejection>,
    ) -> Option<(ComposableCallSite, String)> {
        let found = &self.calls[&call];
        let declined = match found.origin {
            CalleeOrigin::File => None,
            CalleeOrigin::Module => unsupported_module_call(ir, call),
            CalleeOrigin::Library(target) => self.unsupported_library_call(ir, call, target),
        };
        if let Some(what) = declined {
            rejections.push(PluginRejection {
                refusal: true,
                span: found.span.unwrap_or(span),
                message: unsupported(what),
            });
            return None;
        }
        let Some(call_span) = found.span else {
            rejections.push(PluginRejection {
                refusal: true,
                span,
                message: unsupported("a composable call without a source position"),
            });
            return None;
        };
        let IrExpr::Call {
            callee,
            args,
            dispatch_receiver,
            ..
        } = &ir.exprs[call as usize]
        else {
            unreachable!("a composable call is a call");
        };
        // A library callee takes no function value, so only the file's own callee is passed one.
        let function = (found.origin == CalleeOrigin::File).then(|| {
            callee
                .source_function()
                .expect("a call of the file's composable function names it")
        });
        // A library extension's receiver is its dispatch receiver, ahead of the arguments. An
        // extension of the file takes it as its first argument.
        let receiver = match found.origin {
            CalleeOrigin::Library(_) => *dispatch_receiver,
            _ => None,
        };
        let file_receiver = function.is_some_and(|function| {
            ir.package_functions.iter().any(|declaration| {
                declaration.function == function && declaration.receiver.is_some()
            })
        });
        let omitted = omitted_parameters(callee);
        let mut supplied = supplied_arguments(callee, args).peekable();
        let arguments = receiver
            .into_iter()
            .map(|receiver| {
                if matches!(ir.exprs[receiver as usize], IrExpr::Lambda { .. })
                    || values.reads_captured_local(ir, receiver)
                {
                    return None;
                }
                argument_state(ir, receiver, values)
            })
            .chain((0..(args.len() + omitted.len()) as u32).map(|parameter| {
                if omitted.contains(&parameter) {
                    return Some(ArgumentState::Omitted);
                }
                let (_, argument) = supplied.next()?;
                // A composable lambda is memoized, the same value in every execution.
                if matches!(ir.exprs[argument as usize], IrExpr::Lambda { .. }) {
                    // The body an inline call places is no value the callee can compare.
                    if self.places_lambda(ir, call, argument) {
                        return Some(ArgumentState::Uncertain);
                    }
                    return composable_lambda_parameter(
                        ir,
                        self.source,
                        callee,
                        parameter,
                        self.composable,
                    )
                    .then_some(ArgumentState::Static);
                }
                // A captured local's state is its declaration's, which a lambda's body cannot see.
                if values.reads_captured_local(ir, argument) {
                    return None;
                }
                argument_state(ir, argument, values)
            }))
            .collect::<Option<Vec<_>>>();
        let Some(arguments) = arguments else {
            rejections.push(PluginRejection {
                refusal: true,
                span: call_span,
                message: unsupported(
                    "a composable call argument that is not a constant, a parameter, or \
                     computed from a parameter",
                ),
            });
            return None;
        };
        let library_mask = match found.origin {
            CalleeOrigin::Library(target) => self
                .source
                .checked
                .dependency_shape(target)
                .is_some_and(|shape| shape.defaults),
            _ => false,
        };
        Some((
            ComposableCallSite {
                call,
                arguments,
                library_mask,
                receiver: receiver.is_some() || file_receiver,
                stability: Vec::new(),
            },
            call_location(self.positions, found.own_span.unwrap_or(call_span)),
        ))
    }

    /// What a call to the library composable `target` does that the pass does not cover yet.
    fn unsupported_library_call(
        &self,
        ir: &IrFile,
        call: ExprId,
        target: crate::ir::ExternalCallableId,
    ) -> Option<&'static str> {
        let IrExpr::Call {
            callee,
            args,
            dispatch_receiver,
            ..
        } = &ir.exprs[call as usize]
        else {
            unreachable!("a composable call is a call");
        };
        let Callee::External {
            params,
            ret,
            extension_receiver_parameter,
            ..
        } = &**callee
        else {
            unreachable!("a library composable call selects a dependency declaration");
        };
        let Some(shape) = self.source.checked.dependency_shape(target) else {
            return Some(
                "a call to a library composable function the compilation does not declare",
            );
        };
        if shape.context_parameters
            || extension_receiver_parameter.is_some()
            || dispatch_receiver.is_some() != shape.extension
        {
            Some("a call to a library composable function with a receiver")
        } else if (0..params.len()).any(|index| {
            matches!(params[index].kind(), TyKind::Fun(_))
                && !composable_lambda_parameter(
                    ir,
                    self.source,
                    callee,
                    index as u32,
                    self.composable,
                )
        }) {
            Some(
                "a call to a library composable function with a function-typed parameter that is \
                 not a composable function type",
            )
        } else if supplied_arguments(callee, args).any(|(parameter, argument)| {
            // A placed lambda is invoked with its captures alone: its own parameters and its
            // receiver have no values in its caller to place.
            self.places_lambda(ir, call, argument)
                && params.get(parameter as usize).is_some_and(|ty| {
                    matches!(ty.kind(), TyKind::Fun(signature) if !signature.params.is_empty())
                })
        }) {
            Some(
                "a call to an inline library composable function with a composable lambda that \
                 takes parameters or has a receiver",
            )
        } else if let Some(placement) = shape
            .inline
            .then(|| {
                supplied_arguments(callee, args)
                    .filter(|&(_, argument)| {
                        matches!(ir.exprs[argument as usize], IrExpr::Lambda { .. })
                    })
                    .map(|(_, argument)| self.lambda_placement(ir, call, argument))
                    .find(|placement| *placement != Some(LambdaPlacement::Placed))
            })
            .flatten()
        {
            Some(match placement {
                None => {
                    "a call to an inline library composable function that publishes no inline \
                     facts for a lambda's parameter"
                }
                Some(LambdaPlacement::Crossinline) => {
                    "a call to an inline library composable function with a lambda passed to a \
                     `crossinline` parameter"
                }
                _ => {
                    "a call to an inline library composable function with a lambda passed to a \
                     parameter it does not inline"
                }
            })
        } else if !matches!(ret.kind(), TyKind::Unit) {
            Some("a call to a library composable function with a result")
        } else if !takes_composer_frame(&shape.synthetic_parameters, params.len(), shape.defaults) {
            Some(
                "a call to a library composable function whose entry does not take the composer, \
                 `$changed` and `$default` parameters",
            )
        } else {
            None
        }
    }

    /// The facts of the composable lambda `argument` passes, or `None` after rejecting its body.
    /// The composable calls in it join `direct_calls`.
    fn composable_lambda(
        &self,
        ir: &IrFile,
        argument: &LambdaArgument,
        identity: LambdaIdentity<'_>,
        direct_calls: &mut crate::fx_hash::HashSet<ExprId>,
        rejections: &mut Vec<PluginRejection>,
        lambdas: &mut Vec<ComposableLambda>,
    ) -> Option<ComposableLambda> {
        let LambdaIdentity {
            key,
            memoization,
            qualified_caller,
            caller_parameters,
            path,
            callee,
            sibling,
        } = identity;
        if matches!(memoization, Memoization::Inline { .. }) {
            return self.placed_lambda(
                ir,
                argument,
                LambdaIdentity {
                    key,
                    memoization,
                    qualified_caller,
                    caller_parameters,
                    path,
                    callee,
                    sibling,
                },
                direct_calls,
                rejections,
            );
        }
        let IrExpr::Lambda { captures, .. } = &ir.exprs[argument.lambda as usize] else {
            unreachable!("a lambda argument is a lambda");
        };
        let local_captures = captures
            .iter()
            .map(
                |&capture| match (&ir.exprs[capture as usize], memoization) {
                    (
                        &IrExpr::GetValue(index),
                        Memoization::Remembered { caller } | Memoization::Instance { caller },
                    ) => index as usize >= ir.functions[caller as usize].params.len(),
                    _ => false,
                },
            )
            .collect::<Vec<_>>();
        let captures = captures.len() as u32;
        let span = self
            .source
            .expression_span(ir, argument.lambda)
            .unwrap_or(crate::diag::Span::new(0, 0));
        let reject = |rejections: &mut Vec<PluginRejection>, what: &str| {
            rejections.push(PluginRejection {
                refusal: true,
                span,
                message: unsupported(what),
            });
        };
        // The form of the body kept for inlining goes with the lambda, calls and all.
        if let IrExpr::Lambda {
            inline_body: Some(copy),
            ..
        } = ir.exprs[argument.lambda as usize]
        {
            let mut pending = vec![copy];
            while let Some(expression) = pending.pop() {
                if self.calls.contains_key(&expression) {
                    direct_calls.insert(expression);
                }
                crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
            }
        }
        let Some(body) = lambda_body(ir, argument.implementation) else {
            reject(rejections, "a composable lambda whose body is not a block");
            return None;
        };
        let direct = body
            .statements
            .iter()
            .copied()
            .map(|statement| initializing_call(ir, statement, self.calls))
            .filter(|call| self.calls.contains_key(call))
            .collect::<Vec<_>>();
        direct_calls.extend(direct.iter().copied());
        // Composable lambdas the body passes are keyed under this lambda's path, and a capturing
        // one is remembered in the composition this lambda's group opens.
        let nested = lambda_arguments(ir, self.source, &direct, self.composable);
        if let Some(what) = unsupported_body_expression(ir, body.block, &nested, &[]) {
            reject(rejections, what);
            return None;
        }
        // A singleton is moved to the singletons class, while a lambda it remembers stays numbered
        // in the function that writes both.
        if memoization == Memoization::Singleton && nested.iter().any(|inner| inner.captures) {
            reject(
                rejections,
                "a composable lambda that captures nothing passing a capturing composable lambda",
            );
            include_lambda_calls(ir, &nested, self.calls, direct_calls);
            return None;
        }
        let path = identity.path.lambda_path(
            &argument.signature,
            identity.callee.unwrap_or_default(),
            identity.sibling,
            argument.parameter,
        );
        let inner_owner = match memoization {
            Memoization::Singleton => format!("{}.lambda${key}", self.singletons),
            Memoization::Remembered { .. } | Memoization::Instance { .. } => {
                qualified_caller.to_string()
            }
            Memoization::Inline { .. } => unreachable!("a placed lambda returned above"),
        };
        let qualified = format!("{inner_owner}.<anonymous>");
        let within = KeyPath {
            signature: "",
            package: "",
            file_name: "",
            within: Some(&path),
        };
        self.register_lambdas(
            ir,
            LambdaOwner {
                path: &within,
                qualified: &qualified,
                caller: argument.implementation,
                caller_parameters: ir.functions[argument.implementation as usize].params.len()
                    as u32,
                direct: &direct,
                arguments: &nested,
                composable: true,
            },
            direct_calls,
            rejections,
            lambdas,
        );
        let Some(first) = body
            .statements
            .first()
            .and_then(|&statement| self.source.expression_span(ir, statement))
        else {
            reject(rejections, "an empty composable lambda");
            return None;
        };
        if span.hi == 0 {
            reject(rejections, "a composable lambda without a source position");
            return None;
        }
        let mut calls = Vec::with_capacity(direct.len());
        let mut locations = Vec::with_capacity(direct.len());
        for &call in &direct {
            if let Some((site, location)) = self.direct_call_site(
                ir,
                call,
                CallValues::Captures(
                    &local_captures,
                    ir.functions[argument.implementation as usize].params.len() as u32 - captures,
                ),
                span,
                rejections,
            ) {
                // A remembered lambda's group comes before the call it is passed to.
                locations.extend(
                    nested
                        .iter()
                        .filter(|inner| inner.call == call && inner.captures)
                        .filter_map(|inner| self.source.expression_span(ir, inner.lambda))
                        .map(|lambda| call_location(self.positions, lambda)),
                );
                calls.push(site);
                locations.push(location);
            }
        }
        let parameters = match lambda_parameters(
            ir,
            self.runtime,
            argument.implementation,
            body.block,
            captures,
        ) {
            Ok(parameters) => parameters,
            Err(what) => {
                rejections.push(PluginRejection {
                    refusal: true,
                    span,
                    message: unsupported(what),
                });
                return None;
            }
        };
        let source_information = self.options.source_information.then(|| {
            // `parameterNameInformation`: the names of the values the lambda is called with,
            // but for its receiver.
            let names = parameters
                .iter()
                .skip(usize::from(argument.receiver))
                .map(|parameter| parameter.name.as_str())
                .collect::<Vec<_>>();
            let names = if names.is_empty() {
                String::new()
            } else {
                format!("N({})", names.join(","))
            };
            format!("C{names}{}:{}", locations.join(","), self.file_reference)
        });
        let positions = self.positions;
        // kotlinc names a singleton after its property, and a remembered lambda after its caller.
        let owner = match memoization {
            Memoization::Singleton => format!("{}.lambda${key}", self.singletons),
            Memoization::Remembered { .. }
            | Memoization::Instance { .. }
            | Memoization::Inline { .. } => qualified_caller.to_string(),
        };
        Some(ComposableLambda {
            lambda: argument.lambda,
            call: argument.call,
            memoization,
            captures,
            parameters,
            source_information,
            implementation: argument.implementation,
            key,
            offsets: (
                positions.unit(span.lo) as i32,
                positions.unit(span.hi) as i32,
            ),
            // kotlinc names the line the body starts on, its first statement's.
            trace: self.options.trace_markers.then(|| {
                format!(
                    "{owner}.<anonymous> ({}:{})",
                    self.source.file_name,
                    positions.line(first.lo)
                )
            }),
            key_meta: self.options.function_key_meta,
            opening_line: positions.line(span.lo) + 1,
            calls,
            inline_template: None,
        })
    }
}

/// A use of a composable `() -> Unit` parameter the transform does not handle yet: anything but
/// calling it, or passing it to a composable lambda parameter, as a statement of the body.
fn unsupported_lambda_use(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    block: ExprId,
    direct: &[ExprId],
    lambdas: &[Option<u32>],
    composable: TypeName,
) -> Option<&'static str> {
    let mut covered = crate::fx_hash::HashSet::default();
    for &statement in direct {
        match &ir.exprs[statement as usize] {
            IrExpr::InvokeFunction { func, .. } => {
                covered.insert(*func);
            }
            IrExpr::Call { callee, args, .. } => covered.extend(
                supplied_arguments(callee, args)
                    .filter(|&(parameter, _)| {
                        composable_lambda_parameter(ir, source, callee, parameter, composable)
                    })
                    .map(|(_, argument)| argument),
            ),
            _ => {}
        }
    }
    let mut pending = vec![block];
    while let Some(expression) = pending.pop() {
        if let IrExpr::GetValue(index) = ir.exprs[expression as usize] {
            if lambdas.get(index as usize).is_some_and(Option::is_some)
                && !covered.contains(&expression)
            {
                return Some(
                    "a composable lambda parameter used other than by calling it or passing it \
                     to a composable lambda parameter",
                );
            }
        }
        push_body_children(ir, expression, &mut pending);
    }
    None
}

/// Queue the operands of `expression` that are in its function's value namespace: a lambda's
/// captures, not the template of its body kept for inlining, whose values are the lambda's own.
fn push_body_children(ir: &IrFile, expression: ExprId, pending: &mut Vec<ExprId>) {
    match &ir.exprs[expression as usize] {
        IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
        _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
    }
}

/// The values the `invoke` of parameter `index` of `function` takes before the composer, the
/// receiver counted, when it is declared a composable function type returning `Unit` and taking
/// fewer than ten values, the composable function types whose calling convention the transform
/// implements: ten or more need a second `$changed` word.
fn composable_arity(
    ir: &IrFile,
    function: FunId,
    index: usize,
    composable: TypeName,
) -> Option<u32> {
    // `index` counts the declared value parameters, so an extension receiver ahead of them in the
    // physical parameters is skipped.
    let receiver = usize::from(ir.extension_receiver_fns.contains(&function));
    let &ty = ir.functions[function as usize]
        .params
        .get(index + receiver)?;
    let TyKind::Fun(signature) = ty.kind() else {
        return None;
    };
    let spelled = ir
        .fn_declared_spellings
        .get(&function)
        .and_then(|spellings| spellings.params.get(index))?;
    (matches!(signature.ret.kind(), TyKind::Unit)
        && signature.context_count == 0
        && !signature.suspend
        && signature.params.len() < SLOTS_PER_INT
        && matches!(
            spelled.annotations.as_slice(),
            [crate::ir::TypeUseAnnotation::Checked(checked)] if checked.annotation == composable
        )
        && spelled.expansion_annotations.is_empty()
        && !spelled
            .args
            .iter()
            .any(|argument| spelled_mentions(argument, composable)))
    .then_some(signature.params.len() as u32)
}

/// The expression of `statement` that makes its composable call: the statement itself, or the
/// initializer of the local `val` it declares.
fn initializing_call(
    ir: &IrFile,
    statement: ExprId,
    calls: &HashMap<ExprId, ComposableCall>,
) -> ExprId {
    match ir.exprs[statement as usize] {
        IrExpr::Variable {
            init: Some(init), ..
        } if calls.contains_key(&init) => init,
        _ => statement,
    }
}

/// A default argument the pass does not apply yet: one that is not a constant, which kotlinc
/// evaluates in a defaults group of its own.
fn unsupported_defaults(ir: &IrFile, function: FunId) -> Option<&'static str> {
    let defaults = ir.param_defaults(function)?;
    // One `$default` int holds a bit per parameter.
    if defaults.len() > 31 {
        return Some("a composable function with default arguments and more than 31 parameters");
    }
    (ir.param_defaults_stub_only(function)
        || defaults
            .iter()
            .flatten()
            .any(|&default| !matches!(ir.exprs[default as usize], IrExpr::Const(_))))
    .then_some("a composable function with a default argument that is not a constant")
}

/// Which of the `count` values from `first` — a function's parameters, or the values its lambda is
/// called with — `block` reads.
fn used_parameters(ir: &IrFile, block: ExprId, first: u32, count: u32) -> Vec<bool> {
    let mut used = vec![false; count as usize];
    let mut pending = vec![block];
    while let Some(expression) = pending.pop() {
        if let IrExpr::GetValue(index) = ir.exprs[expression as usize] {
            if (first..first + count).contains(&index) {
                used[(index - first) as usize] = true;
            }
        }
        push_body_children(ir, expression, &mut pending);
    }
    used
}

/// The values the lambda `implementation` is called with, after its `captures`.
fn lambda_parameters(
    ir: &IrFile,
    runtime: &dyn PluginRuntime,
    implementation: FunId,
    block: ExprId,
    captures: u32,
) -> Result<Vec<TrackedParameter>, &'static str> {
    let declared = &ir.functions[implementation as usize].params[captures as usize..];
    let used = used_parameters(ir, block, captures, declared.len() as u32);
    let identities = ir
        .fn_params
        .get(&implementation)
        .map(|parameters| &parameters.identities[captures as usize..]);
    declared
        .iter()
        .zip(used)
        .enumerate()
        .map(|(index, (&ty, used))| {
            Ok(TrackedParameter {
                name: identities
                    .and_then(|identities| identities.get(index))
                    .and_then(|identity| identity.source_name.clone())
                    .unwrap_or_default(),
                ty,
                used,
                default: None,
                lambda: None,
                stability: super::class_stability::parameter_stability(ir, runtime, ty)?,
            })
        })
        .collect()
}

/// The values a body's composable calls read: a function's parameters, or a lambda's captures.
#[derive(Clone, Copy)]
enum CallValues<'a> {
    /// A function with this many value parameters.
    Parameters(u32),
    /// A lambda's captures, each flagged when it captures a local variable, not a parameter, and
    /// the number of values it is itself called with, which follow them.
    Captures(&'a [bool], u32),
}

impl CallValues<'_> {
    fn count(self) -> u32 {
        match self {
            CallValues::Parameters(count) => count,
            CallValues::Captures(locals, parameters) => locals.len() as u32 + parameters,
        }
    }

    /// The ordinal of the parameter value `index` is, whose state the body's `$dirty` bits hold: a
    /// function's parameter, or a value its lambda is called with, which follows the captures.
    fn tracked(self, index: u32) -> Option<u32> {
        match self {
            CallValues::Parameters(count) => (index < count).then_some(index),
            CallValues::Captures(locals, parameters) => {
                let captures = locals.len() as u32;
                (captures..captures + parameters)
                    .contains(&index)
                    .then(|| index - captures)
            }
        }
    }

    /// Whether `expression` reads a captured local, whose static-ness kotlinc takes from the
    /// local's declaration (a `val` with a static initializer is static).
    fn reads_captured_local(self, ir: &IrFile, expression: ExprId) -> bool {
        let CallValues::Captures(locals, _) = self else {
            return false;
        };
        let mut pending = vec![expression];
        while let Some(expression) = pending.pop() {
            if let IrExpr::GetValue(index) = ir.exprs[expression as usize] {
                if locals.get(index as usize) == Some(&true) {
                    return true;
                }
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        }
        false
    }
}

/// kotlinc's `argumentMetaOf` for an argument of a call in a function with `parameters` value
/// parameters, or `None` when its answer depends on more than krusty classifies yet.
fn argument_state(ir: &IrFile, argument: ExprId, values: CallValues<'_>) -> Option<ArgumentState> {
    match ir.exprs[argument as usize] {
        IrExpr::Const(_) => Some(ArgumentState::Static),
        // A captured value is not a parameter: its callee compares it itself.
        IrExpr::GetValue(index) if index < values.count() => Some(
            values
                .tracked(index)
                .map_or(ArgumentState::Uncertain, ArgumentState::Forwarded),
        ),
        _ => reads_parameter(ir, argument, values.count()).then_some(ArgumentState::Uncertain),
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

/// The body of a function written `= expression`: the function's root block, which holds only the
/// `return` of that expression.
pub(super) fn expression_body(ir: &IrFile, function: FunId) -> Option<FunctionBody> {
    let root = ir.functions[function as usize].body?;
    let IrExpr::Block { stmts, value: None } = &ir.exprs[root as usize] else {
        return None;
    };
    let [statement] = stmts.as_slice() else {
        return None;
    };
    matches!(ir.exprs[*statement as usize], IrExpr::Return(Some(_))).then(|| FunctionBody {
        block: root,
        statements: vec![*statement],
    })
}

/// A call whose selected declaration is composable.
struct ComposableCall {
    span: Option<crate::diag::Span>,
    /// The call's own syntax, which `sourceInformation` names: a qualified call's selector.
    own_span: Option<crate::diag::Span>,
    origin: CalleeOrigin,
}

/// Where a composable call's callee is declared, which decides who gives it its composer
/// parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalleeOrigin {
    /// This file, whose pass gives the callee its composer parameters.
    File,
    /// Another file of the module.
    Module,
    /// A dependency the Compose compiler plugin compiled, which takes them already.
    Library(crate::ir::ExternalCallableId),
}

/// Every call in `ir` whose selected declaration is composable.
fn composable_calls(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    composable_functions: &crate::fx_hash::HashSet<FunId>,
    composable: TypeName,
) -> HashMap<ExprId, ComposableCall> {
    let local = |function: &FunId| {
        composable_functions
            .contains(function)
            .then_some(CalleeOrigin::File)
    };
    let module = |target: &crate::ir::CallableId| {
        if ir
            .checked_callable_functions
            .get(target)
            .is_some_and(|function| composable_functions.contains(function))
        {
            return Some(CalleeOrigin::File);
        }
        ir.referenced_module_callables
            .get(target)
            .is_some_and(|callable| {
                callable
                    .annotations
                    .iter()
                    .any(|annotation| annotation.identity == composable)
            })
            .then_some(CalleeOrigin::Module)
    };
    ir.exprs
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            let origin = match expression {
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
                        .then_some(CalleeOrigin::Library(*target)),
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
                    own_span: source.call_span(ir, call),
                    origin,
                },
            ))
        })
        .collect()
}

/// Reject every read of a dependency property whose getter is composable: the pass does not give
/// a getter call the caller's composer and `$changed` yet.
fn reject_composable_property_reads(
    ir: &IrFile,
    source: &PluginSourceView<'_>,
    composable: TypeName,
    rejections: &mut Vec<PluginRejection>,
) {
    for (index, expression) in ir.exprs.iter().enumerate() {
        let IrExpr::Checked(operation) = expression else {
            continue;
        };
        let crate::ir::IrCheckedOperation::ExternalPropertyRead { target, .. } = &**operation
        else {
            continue;
        };
        let composable_getter = source
            .checked
            .dependency_property_getter(*target)
            .is_some_and(|getter| {
                source
                    .checked
                    .dependency_annotations(getter)
                    .contains(&composable)
            });
        if composable_getter {
            rejections.push(PluginRejection {
                refusal: true,
                span: source
                    .expression_span(ir, index as ExprId)
                    .unwrap_or(crate::diag::Span::new(0, 0)),
                message: unsupported("a read of a composable property"),
            });
        }
    }
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
        refusal: true,
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
    match ty.non_null().kind() {
        // A function type is the class kotlinc names by its arity.
        TyKind::Fun(signature) => format!(
            "{}Function{}",
            if signature.suspend { "Suspend" } else { "" },
            signature.params.len()
        ),
        TyKind::Unit => "Unit".to_string(),
        TyKind::Nothing => "Nothing".to_string(),
        TyKind::TyParam(name, _) => name.as_str().to_string(),
        _ => ty
            .kotlin_class_internal()
            .map(|classifier| classifier.nested_segment_ref().to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod composer_frame_tests {
    use super::takes_composer_frame;
    use crate::types::Ty;

    fn composer() -> Ty {
        Ty::obj("androidx/compose/runtime/Composer")
    }

    #[test]
    fn the_composer_and_its_words_are_the_frame() {
        assert!(takes_composer_frame(&[composer(), Ty::Int], 3, false));
        assert!(takes_composer_frame(
            &[composer(), Ty::Int, Ty::Int],
            3,
            true
        ));
    }

    #[test]
    fn a_missing_or_extra_word_is_not() {
        assert!(!takes_composer_frame(&[composer(), Ty::Int], 3, true));
        assert!(!takes_composer_frame(
            &[composer(), Ty::Int, Ty::Int],
            3,
            false
        ));
        assert!(!takes_composer_frame(&[], 3, false));
    }

    #[test]
    fn a_changed_word_follows_every_ten_parameters() {
        assert!(takes_composer_frame(
            &[composer(), Ty::Int, Ty::Int],
            11,
            false
        ));
        assert!(!takes_composer_frame(&[composer(), Ty::Int], 11, false));
        assert!(takes_composer_frame(
            &[composer(), Ty::Int, Ty::Int, Ty::Int],
            12,
            true
        ));
        // One `$default` word covers 31 parameters.
        assert!(!takes_composer_frame(
            &[composer(), Ty::Int, Ty::Int, Ty::Int, Ty::Int],
            32,
            true
        ));
    }

    #[test]
    fn a_wrong_role_is_not() {
        assert!(!takes_composer_frame(
            &[Ty::Long, Ty::Int, Ty::Int],
            3,
            true
        ));
        assert!(!takes_composer_frame(
            &[composer(), Ty::Long, Ty::Int],
            3,
            true
        ));
        assert!(!takes_composer_frame(
            &[Ty::Int, composer(), Ty::Int],
            3,
            true
        ));
        assert!(!takes_composer_frame(
            &[Ty::String, Ty::Long, Ty::Int],
            3,
            true
        ));
    }
}
