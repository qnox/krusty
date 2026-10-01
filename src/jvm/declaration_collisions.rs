//! JVM declaration-signature validation after representation selection.
//!
//! Kotlin declarations can be distinct at the source level while their JVM realizations collide.
//! This pass runs only after representation passes have selected physical method names and types,
//! so FIR and common lowering never depend on JVM descriptors.

use crate::diag::{DiagSink, Span};
use crate::ir::{FunId, IrFile};

pub(super) fn validate(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    diags: &mut DiagSink,
) -> Result<(), ()> {
    let platform_clash = report_platform_declaration_clashes(ir, override_results, diags);
    for class in &ir.classes {
        if let Err(message) = reject_duplicate_constructors(class) {
            diags.error(Span::new(0, 0), message);
            return Err(());
        }
        let Some(properties) = ir.member_ext_props.get(&class.fq_name) else {
            continue;
        };
        for property in properties {
            for accessor in std::iter::once(property.getter).chain(property.setter) {
                if let Err(message) = reject_duplicate_method(ir, class, accessor) {
                    diags.error(Span::new(0, 0), message);
                    return Err(());
                }
            }
        }
    }
    let package_clashes = match package_function_signature_clashes(ir) {
        Ok(clashes) => clashes,
        Err(errors) => {
            for (span, message) in errors {
                diags.error(span, message);
            }
            return Err(());
        }
    };
    let package_clash = !package_clashes.is_empty();
    for (span, message) in package_clashes {
        diags.error(span, message);
    }
    if platform_clash || package_clash {
        Err(())
    } else {
        Ok(())
    }
}

/// Source methods whose physical JVM name and descriptor are identical. Kotlin already rejected
/// members that share a declaration signature, so a group here differs in Kotlin and collides only
/// after erasure. Methods without a recorded `fun` offset are representations, not declarations.
fn report_platform_declaration_clashes(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    diags: &mut DiagSink,
) -> bool {
    let mut clashes = Vec::new();
    for class in &ir.classes {
        let mut groups: std::collections::HashMap<(String, String), Vec<u32>> =
            std::collections::HashMap::new();
        for &method in &class.methods {
            let Some(&offset) = ir.fn_signature_offsets.get(&method) else {
                continue;
            };
            let Some(function) = ir.functions.get(method as usize) else {
                continue;
            };
            let descriptor = crate::jvm::ir_emit::function_descriptor(ir, override_results, method);
            groups
                .entry((function.name.clone(), descriptor))
                .or_default()
                .push(offset);
        }
        for ((name, descriptor), offsets) in groups {
            if offsets.len() < 2 {
                continue;
            }
            let message = format!(
                "platform declaration clash: The following declarations have the same JVM signature ({name}{descriptor}):"
            );
            for offset in offsets {
                clashes.push((offset, message.clone()));
            }
        }
    }
    clashes.sort_by_key(|(offset, _)| *offset);
    let found = !clashes.is_empty();
    for (offset, message) in clashes {
        diags.error(Span::new(offset, offset), message);
    }
    found
}

fn reject_duplicate_constructors(class: &crate::ir::IrClass) -> Result<(), String> {
    let mut descriptors = std::collections::HashSet::new();
    if class.has_primary_ctor {
        descriptors.insert(crate::jvm::names::method_descriptor(
            &crate::jvm::ir_emit::class_ctor_jvm_tys(class),
            crate::types::Ty::Unit,
        ));
    }
    for constructor in &class.secondary_ctors {
        let parameters = crate::jvm::ir_emit::jvm_tys(&constructor.prefix_params)
            .into_iter()
            .chain(crate::jvm::ir_emit::jvm_tys(&constructor.params))
            .collect::<Vec<_>>();
        let descriptor = crate::jvm::names::method_descriptor(&parameters, crate::types::Ty::Unit);
        if !descriptors.insert(descriptor.clone()) {
            return Err(format!(
                "platform declaration clash: '{}' contains duplicate JVM constructor <init>{descriptor}",
                class.fq_name
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_method(
    ir: &IrFile,
    class: &crate::ir::IrClass,
    accessor: FunId,
) -> Result<(), String> {
    let Some(accessor_function) = ir.functions.get(accessor as usize) else {
        return Err("internal error: member extension accessor has no JVM function".to_string());
    };
    let descriptor =
        crate::jvm::names::method_descriptor(&accessor_function.params, accessor_function.ret);
    if class.methods.iter().copied().any(|candidate| {
        candidate != accessor
            && ir
                .functions
                .get(candidate as usize)
                .is_some_and(|function| {
                    function.name == accessor_function.name
                        && crate::jvm::names::method_descriptor(&function.params, function.ret)
                            == descriptor
                })
    }) {
        return Err(format!(
            "platform declaration clash: '{}' contains duplicate JVM method {}{}",
            class.fq_name, accessor_function.name, descriptor
        ));
    }
    Ok(())
}

/// Package functions whose representation passes selected the same JVM method.
///
/// Different Kotlin shapes (`fun <T> id(): Int` and `fun id(): Int`, or `List<T>` and
/// `List<String>`) stay distinct in the resolver. They collide only once the physical name and
/// descriptor are known, and only inside one file facade.
fn package_function_signature_clashes(
    ir: &IrFile,
) -> Result<Vec<(crate::diag::Span, String)>, Vec<(crate::diag::Span, String)>> {
    let mut groups: Vec<((String, String), Vec<usize>)> = Vec::new();
    for (index, declaration) in ir.package_functions.iter().enumerate() {
        if declaration.companion {
            continue;
        }
        let Some(function) = ir.functions.get(declaration.function as usize) else {
            return Err(vec![(
                declaration.signature_span,
                "internal error: package function platform-clash validation has no IR function"
                    .to_string(),
            )]);
        };
        if let Err(kind) = determined_function_signature(function) {
            return Err(vec![(
                declaration.signature_span,
                undetermined_type_message(kind),
            )]);
        }
        let signature = (
            function.name.clone(),
            crate::jvm::names::method_descriptor(&function.params, function.ret),
        );
        if let Some((_, members)) = groups.iter_mut().find(|(key, _)| *key == signature) {
            members.push(index);
        } else {
            groups.push((signature, vec![index]));
        }
    }
    let mut clashes = Vec::new();
    for ((name, descriptor), members) in groups {
        if members.len() < 2 {
            continue;
        }
        let mut rendered = Vec::with_capacity(members.len());
        for &index in &members {
            let declaration = &ir.package_functions[index];
            let key = declaration_order(declaration).map_err(|kind| {
                vec![(declaration.signature_span, undetermined_type_message(kind))]
            })?;
            rendered.push((declaration, key));
        }
        rendered.sort_by(|(_, left), (_, right)| left.cmp(right));
        let listed = rendered
            .iter()
            .map(|(declaration, _)| render_declaration(declaration, ir.package.as_deref()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|kind| {
                vec![(
                    rendered[0].0.signature_span,
                    undetermined_type_message(kind),
                )]
            })?;
        let message = clash_message(&name, &descriptor, &listed);
        let mut located = members
            .into_iter()
            .map(|index| (ir.package_functions[index].signature_span, message.clone()))
            .collect::<Vec<_>>();
        located.sort_by_key(|(span, _)| span.lo);
        clashes.extend(located);
    }
    clashes.sort_by_key(|(span, _)| span.lo);
    Ok(clashes)
}

fn clash_message(name: &str, descriptor: &str, declarations: &[String]) -> String {
    let mut message = format!(
        "platform declaration clash: The following declarations have the same JVM signature ({name}{descriptor}):"
    );
    for declaration in declarations {
        message.push('\n');
        message.push_str("    ");
        message.push_str(declaration);
    }
    message
}

fn value_parameters(
    function: &crate::ir::IrPackageFunction,
) -> Result<&[(String, crate::types::Ty)], UndeterminedDiagnosticType> {
    function
        .params
        .get(function.context_count..)
        .ok_or(UndeterminedDiagnosticType::InvalidContextParameterCount)
}

fn render_declaration(
    function: &crate::ir::IrPackageFunction,
    package: Option<&str>,
) -> Result<String, UndeterminedDiagnosticType> {
    let mut rendered = String::from("fun ");
    if !function.type_params.is_empty() {
        rendered.push('<');
        for (index, parameter) in function.type_params.iter().enumerate() {
            if index > 0 {
                rendered.push_str(", ");
            }
            rendered.push_str(&parameter.name);
            let bounds = parameter
                .bounds
                .iter()
                .copied()
                .filter(|bound| !is_default_bound(*bound))
                .collect::<Vec<_>>();
            if !bounds.is_empty() {
                rendered.push_str(" : ");
                for (bound_index, bound) in bounds.iter().enumerate() {
                    if bound_index > 0 {
                        rendered.push_str(" & ");
                    }
                    rendered.push_str(&source_type_name(*bound)?);
                }
            }
        }
        rendered.push_str("> ");
    }
    if let Some(receiver) = function.receiver {
        rendered.push_str(&source_type_name(receiver)?);
        rendered.push('.');
    }
    rendered.push_str(&function.name);
    rendered.push('(');
    for (index, (name, ty)) in value_parameters(function)?.iter().enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        rendered.push_str(name);
        rendered.push_str(": ");
        rendered.push_str(&source_type_name(*ty)?);
    }
    rendered.push_str("): ");
    rendered.push_str(&source_type_name(function.ret)?);
    rendered.push_str(" defined in ");
    match package {
        Some(package) if !package.is_empty() => rendered.push_str(package),
        _ => rendered.push_str("root package"),
    }
    Ok(rendered)
}

/// kotlinc's `MemberComparator` order: name, then rendered receiver and value-parameter types
/// (classifiers qualified), then type-parameter count.
fn declaration_order(
    function: &crate::ir::IrPackageFunction,
) -> Result<(String, String, Vec<String>, usize), UndeterminedDiagnosticType> {
    Ok((
        function.name.clone(),
        function
            .receiver
            .map(qualified_type_name)
            .transpose()?
            .unwrap_or_default(),
        value_parameters(function)?
            .iter()
            .map(|(_, ty)| qualified_type_name(*ty))
            .collect::<Result<Vec<_>, _>>()?,
        function.type_params.len(),
    ))
}

fn is_default_bound(bound: crate::types::Ty) -> bool {
    let inner = match bound {
        crate::types::Ty::Nullable(inner) => *inner,
        other => other,
    };
    matches!(inner, crate::types::Ty::Obj(name, arguments)
        if arguments.is_empty() && name == crate::types::wk::any())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UndeterminedDiagnosticType {
    Error,
    Pending,
    InvalidContextParameterCount,
}

fn undetermined_type_message(kind: UndeterminedDiagnosticType) -> String {
    match kind {
        UndeterminedDiagnosticType::Error => {
            "internal error: package function platform-clash diagnostic contains an error type"
                .to_string()
        }
        UndeterminedDiagnosticType::Pending => {
            "internal error: package function platform-clash diagnostic contains a pending type"
                .to_string()
        }
        UndeterminedDiagnosticType::InvalidContextParameterCount => {
            "internal error: package function platform-clash diagnostic has an invalid context-parameter count"
                .to_string()
        }
    }
}

fn determined_function_signature(
    function: &crate::ir::IrFunction,
) -> Result<(), UndeterminedDiagnosticType> {
    for &parameter in &function.params {
        ensure_determined_type(parameter)?;
    }
    ensure_determined_type(function.ret)?;
    Ok(())
}

fn source_type_name(ty: crate::types::Ty) -> Result<String, UndeterminedDiagnosticType> {
    ensure_determined_type(ty)?;
    Ok(ty.source_name())
}

fn ensure_determined_type(ty: crate::types::Ty) -> Result<(), UndeterminedDiagnosticType> {
    use crate::types::Ty;
    match ty {
        Ty::Error => Err(UndeterminedDiagnosticType::Error),
        Ty::Pending => Err(UndeterminedDiagnosticType::Pending),
        Ty::Obj(_, arguments) => {
            for &argument in arguments {
                ensure_determined_type(argument)?;
            }
            Ok(())
        }
        Ty::Fun(signature) => {
            for &parameter in &signature.params {
                ensure_determined_type(parameter)?;
            }
            ensure_determined_type(signature.ret)
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner)
        | Ty::StarProjection(inner)
        | Ty::TyParam(_, inner) => ensure_determined_type(*inner),
        _ => Ok(()),
    }
}

fn qualified_type_name(ty: crate::types::Ty) -> Result<String, UndeterminedDiagnosticType> {
    use crate::types::Ty;
    Ok(match ty {
        Ty::Int => "kotlin.Int".to_string(),
        Ty::Byte => "kotlin.Byte".to_string(),
        Ty::Short => "kotlin.Short".to_string(),
        Ty::Long => "kotlin.Long".to_string(),
        Ty::Float => "kotlin.Float".to_string(),
        Ty::Double => "kotlin.Double".to_string(),
        Ty::Boolean => "kotlin.Boolean".to_string(),
        Ty::Char => "kotlin.Char".to_string(),
        Ty::UByte => "kotlin.UByte".to_string(),
        Ty::UShort => "kotlin.UShort".to_string(),
        Ty::UInt => "kotlin.UInt".to_string(),
        Ty::ULong => "kotlin.ULong".to_string(),
        Ty::String => "kotlin.String".to_string(),
        Ty::Unit => "kotlin.Unit".to_string(),
        Ty::Nothing => "kotlin.Nothing".to_string(),
        Ty::Null => "null".to_string(),
        Ty::Obj(name, arguments) => {
            let base = name.render().replace(['/', '$'], ".");
            if arguments.is_empty() {
                base
            } else {
                let arguments = arguments
                    .iter()
                    .map(|argument| qualified_type_name(*argument))
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ");
                format!("{base}<{arguments}>")
            }
        }
        Ty::Nullable(inner) => {
            let rendered = qualified_type_name(*inner)?;
            if matches!(*inner, Ty::Fun(_)) {
                format!("({rendered})?")
            } else {
                format!("{rendered}?")
            }
        }
        Ty::PlatformNullable(inner) => format!("{}!", qualified_type_name(*inner)?),
        Ty::DefinitelyNotNull(inner) => {
            format!("{} & Any", qualified_type_name(*inner)?)
        }
        Ty::TyParam(name, _) => crate::types::type_parameter_source_name(name).to_string(),
        Ty::Fun(signature) => {
            let parameters = signature
                .params
                .iter()
                .map(|parameter| qualified_type_name(*parameter))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            let suspend = if signature.suspend { "suspend " } else { "" };
            format!(
                "{suspend}({parameters}) -> {}",
                qualified_type_name(signature.ret)?
            )
        }
        Ty::InProjection(inner) => format!("in {}", qualified_type_name(*inner)?),
        Ty::OutProjection(inner) => format!("out {}", qualified_type_name(*inner)?),
        Ty::StarProjection(_) => "*".to_string(),
        Ty::Error => return Err(UndeterminedDiagnosticType::Error),
        Ty::Pending => return Err(UndeterminedDiagnosticType::Pending),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_type_rendering_rejects_undetermined_types() {
        assert_eq!(
            qualified_type_name(crate::types::Ty::Error),
            Err(UndeterminedDiagnosticType::Error)
        );
        assert_eq!(
            qualified_type_name(crate::types::Ty::Pending),
            Err(UndeterminedDiagnosticType::Pending)
        );
        assert_eq!(
            qualified_type_name(crate::types::Ty::obj_args(
                "repo/Payload",
                &[crate::types::Ty::Error]
            )),
            Err(UndeterminedDiagnosticType::Error)
        );
        assert_eq!(
            undetermined_type_message(UndeterminedDiagnosticType::Error),
            "internal error: package function platform-clash diagnostic contains an error type"
        );
        assert_eq!(
            undetermined_type_message(UndeterminedDiagnosticType::Pending),
            "internal error: package function platform-clash diagnostic contains a pending type"
        );
    }

    #[test]
    fn diagnostic_type_rendering_preserves_definitely_non_null_identity() {
        let caller = crate::types::Ty::ty_param(
            "T",
            crate::types::Ty::nullable(crate::types::Ty::obj_name(crate::types::wk::any())),
        );
        let intersection = caller.contributed_through_nullable_formal();

        assert_eq!(qualified_type_name(intersection), Ok("T & Any".to_string()));
    }

    #[test]
    fn default_bound_uses_resolved_builtin_identity() {
        assert!(is_default_bound(crate::types::Ty::nullable(
            crate::types::Ty::obj_name(crate::types::wk::any())
        )));
        assert!(!is_default_bound(crate::types::Ty::obj("repo/Any")));
    }
}
