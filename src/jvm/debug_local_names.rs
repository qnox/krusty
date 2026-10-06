//! JVM spelling of backend-neutral common-IR debug-local provenance.

use crate::ir::{ExprId, FunId, IrDebugLocalProvenance, IrFile, IrInlineLocalRole, IrLambdaOrigin};

/// Physical JVM names for specialized lambda implementation copies.
///
/// A lambda origin keeps kotlinc's `$lambda$N` spelling. Common IR retains only semantic expansion
/// provenance; the JVM computes the suffix that makes each copied implementation unique.
pub(super) fn lambda_implementation_names(
    ir: &IrFile,
    facade: &str,
    modes: crate::jvm::ir_emit::LambdaModes,
    specialized_suspend_classes: &crate::jvm::suspend::SpecializedLambdaClasses,
) -> Vec<(u32, String)> {
    ir.functions
        .iter()
        .enumerate()
        .filter_map(|(index, function)| {
            let id = index as u32;
            if realized_lambda_class_method(ir, id, specialized_suspend_classes) {
                return None;
            }
            let specialization = ir.specialized_functions.get(&id)?;
            let origin = ir.lambda_origins.get(&id);
            let mut name = origin.map_or_else(|| function.name.clone(), lambda_implementation_name);
            let ordinal = crate::jvm::ir_emit::lambda_class_names::specialization_ordinal(
                ir, id, facade, modes,
            )
            .expect("a specialized implementation retains its sibling position");
            let (_, caller) = crate::jvm::ir_emit::lambda_class_names::specialization_location(
                ir,
                specialization,
                facade,
                modes,
            )
            .expect("a specialized implementation retains its JVM caller location");
            name = format!("{name}${caller}${ordinal}");
            Some((id, name))
        })
        .collect()
}

/// Give source lambda implementations their source-owned JVM spellings before lifting reparents
/// them. The lifted-name pass may then qualify those names with the final caller. A later
/// specialization pass must never revisit these source functions and erase that qualification.
pub(crate) fn realize_source_lambda_implementation_names(ir: &mut IrFile) {
    let names = ir
        .lambda_origins
        .iter()
        .filter(|(function, _)| !ir.specialized_functions.contains_key(function))
        .map(|(&function, origin)| (function, lambda_implementation_name(origin)))
        .collect::<Vec<_>>();
    for (function, name) in names {
        ir.functions[function as usize].name = name;
    }
}

/// A lambda already moved onto a concrete class owns an exact target method name (`invoke` or
/// `invokeSuspend`). Only emitter-planned lambdas still need a generated implementation spelling.
fn realized_lambda_class_method(
    ir: &IrFile,
    function: FunId,
    specialized_suspend_classes: &crate::jvm::suspend::SpecializedLambdaClasses,
) -> bool {
    specialized_suspend_classes.owns_method(function)
        || ir.classes.iter().any(|class| {
            class
                .lambda
                .as_ref()
                .is_some_and(|lambda| lambda.invoke == function)
        })
}

/// Apply generated-specialization spellings after lambda/suspend realization and lifted naming
/// have fixed each copied implementation's physical caller and final method name.
pub(crate) fn realize_lambda_implementation_names(
    ir: &mut IrFile,
    facade: &str,
    modes: crate::jvm::ir_emit::LambdaModes,
    specialized_suspend_classes: &crate::jvm::suspend::SpecializedLambdaClasses,
) {
    let names = lambda_implementation_names(ir, facade, modes, specialized_suspend_classes);
    for (function, name) in names {
        ir.functions[function as usize].name = name;
    }
}

pub(super) fn lambda_implementation_name(origin: &IrLambdaOrigin) -> String {
    let enclosing = if origin.implementation_name.is_empty() {
        "_init_"
    } else {
        origin.implementation_name.as_str()
    };
    format!("{enclosing}$lambda${}", origin.implementation_ordinal)
}

/// The name a dependency's own debug local carries once its body is spliced into a caller.
///
/// kotlinc suffixes an inlined value with `$iv` and leaves its inline-depth markers (`$i$f$…`,
/// `$i$a$…`) alone — those already name the inlining they belong to. The suffix is a property of
/// being inlined rather than of the declaration, which is why it is applied here and not stored.
pub(super) fn spliced_local_name(name: &str) -> String {
    if name.starts_with("$i$f$") || name.starts_with("$i$a$") {
        name.to_string()
    } else {
        format!("{name}$iv")
    }
}

/// The name of the inline-depth marker a spliced lambda body opens with.
///
/// kotlinc spells it `$i$a$-<inline callee>-<the lambda's own class>`. The lambda is inlined and no
/// such class is emitted, so this string is the only place the name exists. The class is the one
/// the JVM naming pass realized from the lambda's `class_provenance` (`Kt$f$r$1` for a lambda
/// bound to `val r`, `Kt$f$2` in a suspend function whose continuation takes the first position).
/// `None` when that provenance was never realized: the name is not reconstructed from the owner.
pub(super) fn spliced_lambda_marker_name(
    ir: &IrFile,
    callee: &str,
    implementation: FunId,
) -> Option<String> {
    // A debug-table name is an UNQUALIFIED name (JVMS 4.2.2): `/` is illegal in one, and a class
    // loader rejects the whole class over it. The class contributes its simple name only.
    let class = ir.lambda_class_names.get(&implementation)?.segment_ref();
    (!class.is_empty()).then(|| format!("$i$a$-{callee}-{class}"))
}

/// The debug name of the local `declaration` declares in code emitted into the class `owner`: an
/// inline-depth marker's, or any other local's as [`name`] spells it.
pub(super) fn declared_name(ir: &IrFile, declaration: ExprId) -> Option<String> {
    match ir.debug_local_provenance(declaration) {
        Some(provenance) if provenance.is_inline_marker() => marker_name(ir, declaration),
        _ => name(ir, declaration),
    }
}

/// The name of an inline-depth marker local, or `None` for any other local and for a lambda
/// marker whose class provenance was never realized.
fn marker_name(ir: &IrFile, declaration: ExprId) -> Option<String> {
    let callee = ir.value_names.get(&declaration)?;
    match ir.debug_local_provenance(declaration)? {
        IrDebugLocalProvenance::FunctionFrameMarker => Some(format!("$i$f${callee}")),
        IrDebugLocalProvenance::LambdaFrameMarker {
            implementation,
            depth,
        } => {
            let mut name = spliced_lambda_marker_name(ir, callee, implementation)?;
            for _ in 0..depth {
                name.push_str("$iv");
            }
            Some(name)
        }
        IrDebugLocalProvenance::InlineValue { .. }
        | IrDebugLocalProvenance::InlineLambdaReceiver { .. }
        | IrDebugLocalProvenance::InlineCallableReferenceParameter { .. } => None,
    }
}

/// Render one source/debug local at the JVM boundary. Common lowering records source spelling,
/// inline depth, and stable lambda identity; only this module owns kotlinc's `$this$`, `$iv`, and
/// `_u24` conventions.
pub(super) fn name(ir: &IrFile, declaration: ExprId) -> Option<String> {
    render(
        ir,
        ir.value_names.get(&declaration).map(String::as_str),
        ir.debug_local_provenance(declaration),
    )
}

/// A name as kotlinc spells it inside a debug local: every character other than a letter, a digit
/// or `_` becomes `_u` and its hexadecimal code (`$` is `_u24`, `-` is `_u2d`).
pub(super) fn escaped(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_alphanumeric() || character == '_' {
            escaped.push(character);
        } else {
            escaped.push_str(&format!("_u{:x}", u32::from(character)));
        }
    }
    escaped
}

/// Render one debug local from the two facts common IR carries for every binding: the source
/// spelling it was declared with, and its provenance.
///
/// A `catch` parameter is declared by its `IrCatch` rather than by a `Variable` node, so it has no
/// declaration expression the two tables could be keyed by and hands the same facts over directly.
pub(super) fn render(
    ir: &IrFile,
    source: Option<&str>,
    provenance: Option<IrDebugLocalProvenance>,
) -> Option<String> {
    match provenance {
        Some(IrDebugLocalProvenance::InlineValue { role, depth }) => {
            let escaped = escaped(source?);
            let mut rendered = match role {
                IrInlineLocalRole::Value => escaped,
                // kotlinc spells the expanded callable's own `this` `this_`, and the receiver it
                // extends `$this$<callable>` — the source name is the callable's in the second case
                // and unused in the first.
                IrInlineLocalRole::DispatchReceiver => "this_".to_string(),
                IrInlineLocalRole::ExtensionReceiver => format!("$this${escaped}"),
            };
            for _ in 0..depth {
                rendered.push_str("$iv");
            }
            Some(rendered)
        }
        Some(IrDebugLocalProvenance::InlineLambdaReceiver { implementation }) => {
            let origin = ir.lambda_origins.get(&implementation)?;
            // kotlinc spells the receiver after the lambda's lifted name.
            let name = ir
                .lifted_names
                .get(&implementation)
                .cloned()
                .unwrap_or_else(|| lambda_implementation_name(origin));
            Some(format!("$this${}", escaped(&name)))
        }
        Some(IrDebugLocalProvenance::InlineCallableReferenceParameter { ordinal }) => {
            Some(format!("p{ordinal}"))
        }
        // A marker is named by [`marker_name`], which knows the class it is emitted into.
        Some(
            IrDebugLocalProvenance::FunctionFrameMarker
            | IrDebugLocalProvenance::LambdaFrameMarker { .. },
        ) => None,
        None => source.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrExpr, IrFunction};
    use crate::types::Ty;

    fn local(ir: &mut IrFile, source_name: &str) -> ExprId {
        let index = declaration_index(ir);
        let declaration = ir.add_expr(IrExpr::Variable {
            index,
            ty: Ty::String,
            init: None,
            named: true,
        });
        ir.value_names.insert(declaration, source_name.to_string());
        declaration
    }

    fn declaration_index(ir: &IrFile) -> u32 {
        u32::try_from(ir.exprs.len()).expect("test expression index")
    }

    #[test]
    fn inline_value_escaping_and_depth_are_jvm_owned() {
        let mut ir = IrFile::default();
        let declaration = local(&mut ir, "cost$raw");
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::inline_value(IrInlineLocalRole::Value, 2),
        );
        assert_eq!(name(&ir, declaration).as_deref(), Some("cost_u24raw$iv$iv"));
    }

    #[test]
    fn inline_extension_receiver_uses_the_jvm_receiver_convention() {
        let mut ir = IrFile::default();
        let declaration = local(&mut ir, "map");
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::inline_value(IrInlineLocalRole::ExtensionReceiver, 1),
        );
        assert_eq!(name(&ir, declaration).as_deref(), Some("$this$map$iv"));
    }

    /// The callable's OWN `this` is a different value from the receiver it extends, and kotlinc
    /// spells it differently. A member inline extension binds both, so one spelling cannot serve.
    #[test]
    fn inline_dispatch_receiver_is_spelled_as_the_callables_own_this() {
        let mut ir = IrFile::default();
        let declaration = local(&mut ir, "send");
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::inline_value(IrInlineLocalRole::DispatchReceiver, 1),
        );
        assert_eq!(name(&ir, declaration).as_deref(), Some("this_$iv"));
    }

    fn lambda(ir: &mut IrFile, class_name: Option<&str>) -> FunId {
        let implementation = ir.add_fun(IrFunction {
            name: "$fir_lambda".into(),
            param_checks: Vec::new(),
            params: vec![Ty::String],
            ret: Ty::Int,
            body: None,
            is_static: true,
            dispatch_receiver: None,
        });
        ir.lambda_origins.insert(
            implementation,
            IrLambdaOrigin {
                identity: 0,
                lexical_owner: None,
                enclosing_name: "findResource".into(),
                binding_name: None,
                ordinal: 0,
                implementation_name: "findResource".into(),
                implementation_ordinal: 0,
                receiver_parameter: None,
                label: None,
                form: crate::ir::IrLambdaForm::Literal,
                class_provenance: None,
            },
        );
        if let Some(class_name) = class_name {
            ir.lambda_class_names
                .insert(implementation, crate::types::type_name(class_name));
        }
        implementation
    }

    fn marker(ir: &mut IrFile, callee: &str, provenance: IrDebugLocalProvenance) -> ExprId {
        let declaration = local(ir, callee);
        ir.set_debug_local_provenance(declaration, provenance);
        declaration
    }

    /// A marker is a frame boundary rather than a value: it has no source spelling of its own, so
    /// the plain renderer leaves it to [`marker_name`], and that one names nothing else.
    #[test]
    fn only_the_marker_renderer_names_a_marker() {
        let mut ir = IrFile::default();
        let function = marker(
            &mut ir,
            "twice",
            IrDebugLocalProvenance::FunctionFrameMarker,
        );
        let value = local(&mut ir, "x");
        assert_eq!(name(&ir, function), None);
        assert_eq!(marker_name(&ir, value), None);
    }

    /// An inline function's marker keeps its spelling however deep the expansion it opens is
    /// cloned; a lambda's gains a frame suffix for each enclosing expansion, as a value's does.
    #[test]
    fn a_function_marker_never_nests_and_a_lambda_marker_does() {
        let mut ir = IrFile::default();
        let implementation = lambda(&mut ir, Some("LpKt$g$1"));
        let function = marker(
            &mut ir,
            "twice",
            IrDebugLocalProvenance::FunctionFrameMarker.nested_inline(),
        );
        let spliced = marker(
            &mut ir,
            "twice",
            IrDebugLocalProvenance::LambdaFrameMarker {
                implementation,
                depth: 0,
            }
            .nested_inline(),
        );
        assert_eq!(marker_name(&ir, function).as_deref(), Some("$i$f$twice"));
        assert_eq!(
            marker_name(&ir, spliced).as_deref(),
            Some("$i$a$-twice-LpKt$g$1$iv")
        );
    }

    /// The lambda's class is the naming walk's, realized by the JVM naming pass. A packaged class
    /// contributes only its simple name: a debug-table name may not contain `/`. A lambda whose
    /// provenance was never realized has no name — it is not rebuilt from the owner.
    #[test]
    fn a_spliced_lambda_marker_uses_only_a_realized_class() {
        let mut ir = IrFile::default();
        let named = lambda(&mut ir, Some("lib/Catalog$findResource$r$1"));
        let unnamed = lambda(&mut ir, None);
        assert_eq!(
            spliced_lambda_marker_name(&ir, "firstOrNull", named).as_deref(),
            Some("$i$a$-firstOrNull-Catalog$findResource$r$1")
        );
        assert_eq!(
            spliced_lambda_marker_name(&ir, "firstOrNull", unnamed),
            None
        );
    }

    #[test]
    fn inline_lambda_receiver_uses_the_stable_implementation_origin() {
        let mut ir = IrFile::default();
        let implementation = ir.add_fun(IrFunction {
            name: "$fir_lambda".into(),
            param_checks: Vec::new(),
            params: vec![Ty::String],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
        });
        ir.lambda_origins.insert(
            implementation,
            IrLambdaOrigin {
                identity: 0,
                lexical_owner: None,
                enclosing_name: "nested".into(),
                binding_name: None,
                ordinal: 0,
                implementation_name: "nested".into(),
                implementation_ordinal: 0,
                receiver_parameter: Some(0),
                label: None,
                form: crate::ir::IrLambdaForm::Literal,
                class_provenance: None,
            },
        );
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::String,
            init: None,
            named: true,
        });
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::InlineLambdaReceiver { implementation },
        );
        assert_eq!(
            name(&ir, declaration).as_deref(),
            Some("$this$nested_u24lambda_u240")
        );
    }

    #[test]
    fn a_realized_suspend_lambda_keeps_its_exact_override_name() {
        let mut ir = IrFile::default();
        let implementation = lambda(&mut ir, None);
        ir.lambda_origins.remove(&implementation);
        ir.specialized_functions.insert(
            implementation,
            crate::ir::IrSpecializedFunction {
                source: implementation,
                caller_declaration: crate::fir::DeclarationId::from_raw(0),
                caller: Some(crate::ir::IrEnclosure::File),
                caller_is_default: false,
                caller_source_name: "box".to_string(),
                inline_callee: crate::fir::CallableId::from_raw(0),
                inline_callee_source_name: "define".to_string(),
                parent: None,
            },
        );
        ir.functions[implementation as usize].name = "invokeSuspend".to_string();
        ir.suspend_funs.push(implementation);
        let mut class = crate::ir::IrClass::synthetic(crate::types::type_name("LpKt$use$1"));
        class.methods.push(implementation);
        let class = ir.add_class(class);
        let mut specialized_suspend_classes =
            crate::jvm::suspend::SpecializedLambdaClasses::default();
        specialized_suspend_classes.record(implementation, class);

        realize_lambda_implementation_names(
            &mut ir,
            "LpKt",
            crate::jvm::ir_emit::LambdaModes::default(),
            &specialized_suspend_classes,
        );

        assert_eq!(ir.functions[implementation as usize].name, "invokeSuspend");
    }

    #[test]
    fn late_specialization_naming_does_not_overwrite_a_lifted_source_name() {
        let mut ir = IrFile::default();
        let implementation = lambda(&mut ir, None);
        ir.functions[implementation as usize].name = "outer$nested$lambda$0".to_string();

        realize_lambda_implementation_names(
            &mut ir,
            "LpKt",
            crate::jvm::ir_emit::LambdaModes::default(),
            &crate::jvm::suspend::SpecializedLambdaClasses::default(),
        );

        assert_eq!(
            ir.functions[implementation as usize].name,
            "outer$nested$lambda$0"
        );
    }
}
