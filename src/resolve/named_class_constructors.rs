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
use crate::libraries::{LibraryMember, LibraryType};
use crate::plugins::{FrontendNamedClass, FrontendNamedClassContext};
use crate::types::{AnnotationValue, ResolvedAnnotation, Ty, TypeName, TypeVariance, Visibility};

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
    publish_type_use_serializer_constructors(index, table, diags, &host);
}

#[derive(Clone)]
enum NamedConstructorTarget {
    Module(crate::fir::DeclarationId),
    External(crate::fir::ExternalCallableId),
}

#[derive(Clone)]
struct NamedConstructorCandidate {
    facts: FrontendNamedClass,
    parameters: Vec<Ty>,
    target: NamedConstructorTarget,
}

/// Select every serializer-class construction named by a checked annotation on a property's
/// expanded semantic type. Equal serializer/arity pairs share one primary-constructor decision;
/// neither the plugin nor lowering recovers it from the source spelling.
fn publish_type_use_serializer_constructors(
    index: &mut ResolvedModuleIndex,
    table: &SymbolTable,
    diags: &mut DiagSink,
    host: &crate::plugins::PluginHost,
) {
    let properties = table
        .stable_declared_spellings
        .iter()
        .filter_map(|(&declaration, spellings)| {
            let anchor = index.declaration_anchor(declaration)?;
            if anchor.kind != crate::fir::DeclarationKind::Property {
                return None;
            }
            Some((
                declaration,
                anchor.source.raw(),
                index.signature(declaration)?.result.get(),
                spellings.ret.clone(),
            ))
        })
        .collect::<Vec<_>>();
    for (declaration, source, ty, spelling) in properties {
        let mut path = Vec::new();
        let declaration_annotations = index.declaration_applied_annotations(declaration).to_vec();
        let mut publisher = TypeUseConstructorPublisher {
            index,
            table,
            diags,
            host,
            declaration,
            source,
        };
        // A property annotation and an annotation on its root type both select a serializer for
        // the same checked property type. The declaration occurrence has already been folded into
        // stable metadata; it needs the same exact constructor plan as a type-use occurrence.
        publisher.publish_applications(ty, &declaration_annotations, &[]);
        publisher.publish_node(ty, &spelling, &mut path);
    }
}

struct TypeUseConstructorPublisher<'a> {
    index: &'a mut ResolvedModuleIndex,
    table: &'a SymbolTable,
    diags: &'a mut DiagSink,
    host: &'a crate::plugins::PluginHost,
    declaration: crate::fir::DeclarationId,
    source: u32,
}

impl TypeUseConstructorPublisher<'_> {
    fn publish_applications(
        &mut self,
        ty: Ty,
        applications: &[ResolvedAnnotation],
        spans: &[(TypeName, Span)],
    ) {
        let candidates = named_constructor_candidates(self.table, applications);
        if candidates.is_empty() {
            return;
        }
        let facts = candidates
            .iter()
            .map(|candidate| candidate.facts.clone())
            .collect::<Vec<_>>();
        let type_parameter_count = ty.non_null().type_args().len();
        let (selection, diagnostics) =
            self.host
                .select_named_class_constructor(&FrontendNamedClassContext {
                    annotations: spans,
                    applied_annotations: applications,
                    type_parameter_count,
                    named_classes: &facts,
                });
        self.diags.set_file(self.source);
        for diagnostic in diagnostics {
            self.diags.error(diagnostic.span, diagnostic.message);
        }
        let Some(selection) = selection else {
            return;
        };
        let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.facts.classifier == selection.classifier)
        else {
            return;
        };
        let Ok(parameters) = candidate
            .parameters
            .iter()
            .copied()
            .map(crate::fir::ResolvedTy::new)
            .collect::<Result<Vec<_>, _>>()
        else {
            return;
        };
        let Ok(type_parameter_count) = u32::try_from(type_parameter_count) else {
            return;
        };
        let target = match candidate.target {
            NamedConstructorTarget::Module(declaration) => {
                crate::fir::ResolvedCustomSerializerConstructorTarget::Module(declaration)
            }
            NamedConstructorTarget::External(declaration) => {
                crate::fir::ResolvedCustomSerializerConstructorTarget::External(declaration)
            }
        };
        self.index
            .publish_serialization_type_use_serializer_constructor(
                type_parameter_count,
                crate::fir::ResolvedTypeUseSerializerConstruction {
                    serializer: selection.classifier,
                    parameters: parameters.into_boxed_slice(),
                    operands: selection.operands,
                    target,
                },
            );
    }

    fn publish_node(&mut self, ty: Ty, spelling: &crate::spelling::Spelled, path: &mut Vec<u32>) {
        let applications = spelling
            .annotations
            .iter()
            .chain(&spelling.expansion_annotations)
            .map(|annotation| annotation.checked().clone())
            .collect::<Vec<_>>();
        if !applications.is_empty() {
            let spans = applications
                .iter()
                .filter_map(|application| {
                    self.table
                        .type_use_annotation_spans
                        .get(&(
                            self.declaration,
                            path.clone().into_boxed_slice(),
                            application.annotation,
                        ))
                        .copied()
                        .map(|span| (application.annotation, span))
                })
                .collect::<Vec<_>>();
            self.publish_applications(ty, &applications, &spans);
        }
        for (ordinal, (&argument, argument_spelling)) in ty
            .non_null()
            .type_args()
            .iter()
            .zip(&spelling.args)
            .enumerate()
        {
            let Ok(ordinal) = u32::try_from(ordinal) else {
                continue;
            };
            path.push(ordinal);
            self.publish_node(
                argument.projection_inner().unwrap_or(argument),
                argument_spelling,
                path,
            );
            path.pop();
        }
    }
}

fn named_constructor_candidates(
    table: &SymbolTable,
    applications: &[ResolvedAnnotation],
) -> Vec<NamedConstructorCandidate> {
    let classifiers = applications
        .iter()
        .flat_map(|application| &application.arguments)
        .filter_map(|(_, value)| match value {
            AnnotationValue::Class(ty) => ty.kotlin_class_internal(),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    classifiers
        .into_iter()
        .filter_map(|classifier| named_constructor_candidate(table, classifier))
        .collect()
}

fn named_constructor_candidate(
    table: &SymbolTable,
    classifier: TypeName,
) -> Option<NamedConstructorCandidate> {
    if let Some(class) = table.classes.get(&classifier) {
        if class.is_object() || !class.has_primary_ctor {
            return None;
        }
        let constructor = class.primary_constructor_declaration?;
        return Some(NamedConstructorCandidate {
            facts: named_class(class),
            parameters: class.ctor_params.clone(),
            target: NamedConstructorTarget::Module(constructor),
        });
    }
    let shape = table.libraries.classifier(classifier)?;
    if shape.is_object() {
        return None;
    }
    let constructor = shape
        .constructors
        .iter()
        .find(|constructor| constructor.is_primary_constructor())?;
    // A dependency constructor that is not public is not callable from generated code in this
    // module. Excluding it from the plugin's candidate facts prevents an inaccessible declaration
    // from becoming a late `new X()` fallback.
    if constructor.visibility != Visibility::Public {
        return None;
    }
    Some(NamedConstructorCandidate {
        facts: library_named_class(classifier, &shape, constructor),
        parameters: constructor.params.clone(),
        target: NamedConstructorTarget::External(constructor.external_identity?),
    })
}

fn library_named_class(
    classifier: TypeName,
    shape: &LibraryType,
    constructor: &LibraryMember,
) -> FrontendNamedClass {
    let formals = shape.type_params();
    let name = Ty::obj_name(classifier).source_name();
    let display_name = if formals.is_empty() {
        name
    } else {
        format!("{name}<{}>", vec!["*"; formals.len()].join(", "))
    };
    let primary_constructor = constructor
        .params
        .iter()
        .copied()
        .enumerate()
        .map(|(ordinal, parameter)| {
            let name = constructor
                .call_sig
                .param_names
                .get(ordinal)
                .cloned()
                .unwrap_or_else(|| format!("parameter {ordinal}"));
            (name, parameter)
        })
        .collect();
    let supertype_arguments = shape
        .supertype_templates
        .iter()
        .filter_map(|supertype| {
            let owner = supertype.non_null().kotlin_class_internal()?;
            Some((
                owner,
                supertype
                    .non_null()
                    .type_args()
                    .iter()
                    .map(|argument| argument.source_name())
                    .collect(),
            ))
        })
        .collect();
    FrontendNamedClass {
        classifier,
        display_name,
        primary_constructor,
        supertype_arguments,
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
