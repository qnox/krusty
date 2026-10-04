//! Plugin selection of the constructor of a class a source classifier's annotation names.
//!
//! kotlinx.serialization's `@Serializable(with = X::class)` may name a serializer CLASS, which the
//! classifier's generated `serializer(…)` accessor constructs. The plugin selects that constructor
//! and validates it against its contract, reporting a broken one at the annotation; both decisions
//! are made here, once, while every source declaration and its stable identity is live. The
//! selected constructor and its operand mapping are published by stable declaration identity, and
//! the backend plugin only realizes that recorded choice.
//!
//! This module supplies the plugin with neutral declaration facts — the classifier's resolved
//! annotations and, for each source class a class-valued argument names, its primary constructor
//! and diagnostic renderings. It knows nothing of any one plugin's contract.

use super::{ClassSig, Decl, File, SymbolTable};
use crate::diag::{DiagSink, Span};
use crate::fir::ResolvedModuleIndex;
use crate::plugins::{FrontendNamedClass, FrontendNamedClassContext};
use crate::types::{AnnotationValue, Ty, TypeVariance};

/// Let the active plugins select, validate and publish the constructor of every source class a
/// source classifier's annotations name.
pub(crate) fn publish_named_class_constructors(
    files: &[File],
    index: &mut ResolvedModuleIndex,
    table: &SymbolTable,
    diags: &mut DiagSink,
) {
    if table.native_plugins.is_empty() {
        return;
    }
    let host = table.native_plugins.host("main");
    let mut classes = table.classes.values().collect::<Vec<_>>();
    // Diagnostics follow source order, whatever order the class map iterates in.
    classes.sort_by_key(|class| (class.source_file, class.stable_declaration));
    for class in classes {
        let Some(declaration) = class.stable_declaration else {
            continue;
        };
        let named_classes = named_classes(table, class);
        if named_classes.is_empty() {
            continue;
        }
        let Some(type_parameter_count) = index
            .classifier_own_type_parameter_count(declaration)
            .map(|count| count as usize)
        else {
            continue;
        };
        let annotations = annotation_entries(files, table, class);
        let (selection, diagnostics) =
            host.select_named_class_constructor(&FrontendNamedClassContext {
                annotations: &annotations,
                applied_annotations: &class.applied_annotations,
                type_parameter_count,
                named_classes: &named_classes,
            });
        diags.set_file(class.source_file);
        for diagnostic in diagnostics {
            diags.error(diagnostic.span, diagnostic.message);
        }
        let Some(selection) = selection else {
            continue;
        };
        let Some(constructor) = table
            .classes
            .get(&selection.classifier)
            .and_then(|named| named.primary_constructor_declaration)
        else {
            continue;
        };
        index.publish_serialization_custom_serializer_constructor(
            declaration,
            constructor,
            selection.operands,
        );
    }
}

/// Every source class with a primary constructor that a class-valued argument of `class`'s
/// checked annotation applications names.
fn named_classes(table: &SymbolTable, class: &ClassSig) -> Vec<FrontendNamedClass> {
    let mut named = Vec::new();
    for value in class
        .applied_annotations
        .iter()
        .flat_map(|application| application.arguments.iter().map(|(_, value)| value))
    {
        let Some(classifier) = (match value {
            AnnotationValue::Class(ty) => ty.kotlin_class_internal(),
            _ => None,
        }) else {
            continue;
        };
        if named
            .iter()
            .any(|existing: &FrontendNamedClass| existing.classifier == classifier)
        {
            continue;
        }
        let Some(target) = table.classes.get(&classifier) else {
            continue;
        };
        if target.is_object()
            || !target.has_primary_ctor
            || target.primary_constructor_declaration.is_none()
        {
            continue;
        }
        named.push(named_class(target));
    }
    named
}

fn named_class(class: &ClassSig) -> FrontendNamedClass {
    let owner = type_parameter_owner(class);
    let formals = class.type_params();
    let render = |ty: Ty| {
        ty.source_name_with_type_parameter_in(&[], &|parameter| {
            let source = crate::types::type_parameter_source_name(parameter);
            if formals.iter().any(|formal| formal == parameter) {
                format!("{source} (of {owner})")
            } else {
                source.to_string()
            }
        })
    };
    let name = Ty::obj_name(class.internal).source_name();
    let display_name = if formals.is_empty() {
        name
    } else {
        format!("{name}<{}>", vec!["*"; formals.len()].join(", "))
    };
    let mut supertype_arguments = class
        .interfaces
        .iter()
        .zip(&class.interface_type_args)
        .map(|(interface, arguments)| {
            (
                interface,
                arguments.iter().map(|&argument| render(argument)).collect(),
            )
        })
        .collect::<Vec<_>>();
    if let Some(superclass) = class.super_internal {
        supertype_arguments.push((
            superclass,
            class
                .super_type_args
                .iter()
                .map(|&argument| render(argument))
                .collect(),
        ));
    }
    FrontendNamedClass {
        classifier: class.internal,
        display_name,
        primary_constructor: class
            .ctor_param_names
            .iter()
            .map(|(name, _)| name.clone())
            .zip(class.ctor_params.iter().copied())
            .collect(),
        supertype_arguments,
    }
}

/// `class`'s resolved annotations with the span of each source annotation entry. The entry begins
/// at its `@`, which directly precedes the classifier reference the occurrence's span covers.
fn annotation_entries(
    files: &[File],
    table: &SymbolTable,
    class: &ClassSig,
) -> Vec<(crate::types::TypeName, Span)> {
    let Some(Decl::Class(declaration)) = class
        .source_decl
        .and_then(|decl| Some(files.get(class.source_file as usize)?.decl(decl)))
    else {
        return Vec::new();
    };
    declaration
        .annotations
        .iter()
        .filter_map(|annotation| {
            let identity = table.resolved_annotation(class.source_file, annotation)?;
            Some((
                identity,
                Span::new(annotation.span.lo.saturating_sub(1), annotation.span.hi),
            ))
        })
        .collect()
}

/// kotlinc's owner wording for `class`'s formals: `class Name<out T, U : Bound>`, with an implicit
/// `Any?` bound omitted.
fn type_parameter_owner(class: &ClassSig) -> String {
    let keyword = if class.is_interface() {
        "interface"
    } else {
        "class"
    };
    let formals = class
        .type_params()
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let variance = match class.type_param_variances().get(index) {
                Some(TypeVariance::In) => "in ",
                Some(TypeVariance::Out) => "out ",
                Some(TypeVariance::Invariant) | None => "",
            };
            let bounds = class
                .type_param_bounds()
                .get(index)
                .copied()
                .into_iter()
                .chain(
                    class
                        .type_parameter_extra_bounds
                        .get(index)
                        .into_iter()
                        .flatten()
                        .copied(),
                )
                .filter(|bound| *bound != Ty::nullable(Ty::obj("kotlin/Any")))
                .map(Ty::source_name)
                .collect::<Vec<_>>();
            let source = crate::types::type_parameter_source_name(formal);
            if bounds.is_empty() {
                format!("{variance}{source}")
            } else {
                format!("{variance}{source} : {}", bounds.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let name = Ty::obj_name(class.internal).source_name();
    format!("{keyword} {name}<{formals}>")
}
