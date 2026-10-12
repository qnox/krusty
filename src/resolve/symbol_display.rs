//! kotlinc's SYMBOL rendering (`FirDiagnosticRenderers.SYMBOL`) of this compilation's own
//! declarations, read from their finalized stable records.
//!
//! The rendering is the short declaration header: no visibility, modality, or `inline`, `infix`,
//! `operator`, `const`, `lateinit`, `data`, `sealed` or `abstract` modifier. A function reads
//! `context(c: C) suspend fun <reified T : B> R.name(noinline p: P = ...): Ret`, a property
//! `val <T> R.name: Type`, a constructor `Outer.constructor<T, out R>(p: P): Owner<T, R>`, and a
//! classifier `fun interface Name<in T, out R : Any> : Super`.

use crate::fir::{DeclarationFlags, DeclarationId, DeclarationKind, ResolvedModuleIndex};
use crate::types::{InlineParameterModifier, Ty, TypeVariance};

/// The SYMBOL rendering of a source declaration, or `None` for a kind this renderer does not
/// name.
pub(super) fn source_declaration_display(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Option<String> {
    match index.declaration_header(declaration)?.kind {
        DeclarationKind::Function => function_display(index, declaration),
        DeclarationKind::Property => property_display(index, declaration),
        DeclarationKind::Constructor => constructor_display(index, declaration),
        DeclarationKind::Classifier => classifier_display(index, declaration),
        _ => None,
    }
}

fn function_display(index: &ResolvedModuleIndex, declaration: DeclarationId) -> Option<String> {
    let header = index.declaration_header(declaration)?;
    let name = index.declaration_name(declaration)?;
    let callable = index.callable_for_declaration(declaration)?;
    let signature = index.signature(declaration)?;
    let parameters = value_parameters(index, declaration)?;
    let context_count = (callable.shape.context_parameter_count as usize).min(parameters.len());
    let context = context_display(&parameters[..context_count]);
    let suspend = if header.flags.has(DeclarationFlags::SUSPEND) {
        "suspend "
    } else {
        ""
    };
    let type_parameters = type_parameters_display(index, declaration, false);
    let type_parameters = if type_parameters.is_empty() {
        String::new()
    } else {
        format!("{type_parameters} ")
    };
    let receiver = receiver_display(callable.shape.extension_receiver.map(|ty| ty.get()));
    Some(format!(
        "{context}{}{suspend}fun {type_parameters}{receiver}{name}({}): {}",
        actual_modifier(header.flags),
        parameters[context_count..].join(", "),
        signature.result.get().source_name()
    ))
}

fn property_display(index: &ResolvedModuleIndex, declaration: DeclarationId) -> Option<String> {
    let header = index.declaration_header(declaration)?;
    let name = index.declaration_name(declaration)?;
    let property = index.property(index.property_for_declaration(declaration)?)?;
    let signature = index.signature(declaration)?;
    let keyword = if property.mutable { "var" } else { "val" };
    let type_parameters = type_parameters_display(index, declaration, false);
    let type_parameters = if type_parameters.is_empty() {
        String::new()
    } else {
        format!("{type_parameters} ")
    };
    let receiver = receiver_display(property.extension_receiver.map(|ty| ty.get()));
    Some(format!(
        "{}{keyword} {type_parameters}{receiver}{name}: {}",
        actual_modifier(header.flags),
        signature.result.get().source_name()
    ))
}

fn constructor_display(index: &ResolvedModuleIndex, declaration: DeclarationId) -> Option<String> {
    let header = index.declaration_header(declaration)?;
    let owner = header.owner?;
    let owner_header = index.declaration_header(owner)?;
    let parameters = value_parameters(index, declaration)?;
    let callable = index.callable_for_declaration(declaration)?;
    let context_count = (callable.shape.context_parameter_count as usize).min(parameters.len());
    // An inner class's constructor is named through the classifier holding its outer instance.
    let outer = owner_header
        .flags
        .has(DeclarationFlags::INNER)
        .then_some(owner_header.owner)
        .flatten()
        .and_then(|outer| index.classifier_self_type(outer))
        .map(|outer| format!("{}.", outer.source_name()))
        .unwrap_or_default();
    Some(format!(
        "{outer}{}constructor{}({}): {}",
        actual_modifier(header.flags),
        type_parameters_display(index, owner, true),
        parameters[context_count..].join(", "),
        index.classifier_self_type(owner)?.source_name()
    ))
}

/// kotlinc names an implementation of an `expect` header with the `actual` it wrote.
fn actual_modifier(flags: DeclarationFlags) -> &'static str {
    if flags.has(DeclarationFlags::ACTUAL) {
        "actual "
    } else {
        ""
    }
}

fn classifier_display(index: &ResolvedModuleIndex, declaration: DeclarationId) -> Option<String> {
    let header = index.declaration_header(declaration)?;
    let classifier = index.classifier_header(declaration)?;
    let flags = header.flags;
    let kind = if flags.has(DeclarationFlags::ANNOTATION_CLASS) {
        "annotation class"
    } else if flags.has(DeclarationFlags::FUN_INTERFACE) {
        "fun interface"
    } else if flags.has(DeclarationFlags::INTERFACE) {
        "interface"
    } else if flags.has(DeclarationFlags::ENUM) {
        "enum class"
    } else if flags.has(DeclarationFlags::SINGLETON) && flags.has(DeclarationFlags::COMPANION) {
        "companion object"
    } else if flags.has(DeclarationFlags::SINGLETON) {
        "object"
    } else {
        "class"
    };
    let supertypes = classifier
        .declared_supertypes()
        .map(|supertype| supertype.get().source_name())
        .collect::<Vec<_>>();
    let supertypes = if supertypes.is_empty() {
        let implicit = if flags.has(DeclarationFlags::ENUM) {
            format!("Enum<{}>", classifier.classifier.nested_segment_ref())
        } else if flags.has(DeclarationFlags::ANNOTATION_CLASS) {
            "Annotation".to_string()
        } else {
            "Any".to_string()
        };
        implicit
    } else {
        supertypes.join(", ")
    };
    Some(format!(
        "{kind} {}{} : {supertypes}",
        classifier.classifier.nested_segment_ref(),
        type_parameters_display(index, declaration, true),
    ))
}

/// Each declared value parameter, context parameters first, as
/// `[noinline |crossinline ][vararg ]name: Type[ = ...]`. A `vararg` parameter shows its element.
fn value_parameters(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Option<Vec<String>> {
    let callable = index.callable_for_declaration(declaration)?;
    let signature = index.signature(declaration)?;
    Some(
        signature
            .parameters
            .iter()
            .enumerate()
            .map(|(ordinal, ty)| {
                let ordinal =
                    u32::try_from(ordinal).expect("a callable's parameter count fits u32");
                let name = index
                    .callable_parameter_name(callable.id, ordinal)
                    .unwrap_or("_");
                let flags = index
                    .callable_parameter(callable.id, ordinal)
                    .map(|parameter| parameter.flags());
                let vararg = flags.is_some_and(|flags| flags.is_vararg());
                let modifier = match flags.map(|flags| flags.inline_modifier()) {
                    Some(InlineParameterModifier::Noinline) => "noinline ",
                    Some(InlineParameterModifier::Crossinline) => "crossinline ",
                    _ => "",
                };
                let ty = ty.get();
                let ty = if vararg {
                    ty.non_null().array_read_elem().unwrap_or(ty)
                } else {
                    ty
                };
                format!(
                    "{modifier}{}{name}: {}{}",
                    if vararg { "vararg " } else { "" },
                    ty.source_name(),
                    if flags.is_some_and(|flags| flags.has_default()) {
                        " = ..."
                    } else {
                        ""
                    }
                )
            })
            .collect(),
    )
}

fn context_display(parameters: &[String]) -> String {
    if parameters.is_empty() {
        String::new()
    } else {
        format!("context({}) ", parameters.join(", "))
    }
}

fn receiver_display(receiver: Option<Ty>) -> String {
    receiver
        .map(|receiver| format!("{}.", receiver.source_name()))
        .unwrap_or_default()
}

/// `<reified T : Bound, in U>`, or empty when the declaration has no type parameters. A bound is
/// shown unless it is the implicit `Any?`; a variance only for a classifier's own parameters.
fn type_parameters_display(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    with_variance: bool,
) -> String {
    let parameters = (0..)
        .map_while(|ordinal| index.type_parameter(declaration, ordinal))
        .filter_map(|parameter| {
            let header = index.type_parameter_header(parameter)?;
            let name = index.type_parameter_name(parameter)?;
            let variance = match header.flags.variance() {
                TypeVariance::In if with_variance => "in ",
                TypeVariance::Out if with_variance => "out ",
                _ => "",
            };
            let reified = if header.flags.is_reified() {
                "reified "
            } else {
                ""
            };
            let bound = header
                .bounds
                .first()
                .map(|bound| bound.ty.get())
                .filter(|bound| *bound != Ty::nullable(Ty::obj("kotlin/Any")))
                .map(|bound| format!(" : {}", bound.source_name()))
                .unwrap_or_default();
            Some(format!("{variance}{reified}{name}{bound}"))
        })
        .collect::<Vec<_>>();
    if parameters.is_empty() {
        String::new()
    } else {
        format!("<{}>", parameters.join(", "))
    }
}
