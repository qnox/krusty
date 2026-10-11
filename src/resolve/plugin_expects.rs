//! JetBrains Fleet's `expects` compiler plugin, as frontend decisions.
//!
//! Fleet writes a platform-specific function as a common declaration whose whole body is
//! `fleet.util.multiplatform.linkToActual()` plus one top-level function per platform, in the same
//! package, named with the platform suffix (`Jvm`) and annotated `@fleet.util.multiplatform.Actual`.
//! kotlinc's plugin (an `IrGenerationExtension`) replaces the common body with
//! `return <name>Jvm(<parameters>)` and reports four errors: a `linkToActual()` function with no
//! `@Actual` function of that name, one whose `@Actual` functions all have another signature, an
//! `@Actual` function whose name lacks the suffix, and an `@Actual` function no function of the
//! unsuffixed name matches.
//!
//! The checker makes those decisions while the function's body is live. The `linkToActual()` call
//! is recognized by the declaration it resolved to, never by its spelling, and the `@Actual`
//! annotation by its resolved identity. A selected actual is recorded in
//! [`super::TypeInfo::linked_actuals`]; checked FIR builds the replacement body from that decision.
//!
//! Signatures match as kotlinc's IR types do: the same parameter kinds and types, return type and
//! type-parameter variance and bounds, with the actual's type parameters renamed to the expect's
//! by position. IR type equality compares classifiers, nullability and arguments, so a function
//! type's receiver/context annotation does not take part.
//!
//! Only declarations of the module being compiled are linked. kotlinc also searches dependencies;
//! krusty cannot read a dependency function's `@Actual` annotation (a binary-retained annotation
//! outside Kotlin metadata), so a link that would depend on a dependency declaration is reported
//! as unsupported rather than guessed.

use std::collections::HashMap;

use super::*;
use crate::fir::{DeclarationId, DeclarationKind, ResolvedModuleIndex};

const LINK_TO_ACTUAL: &str = "linkToActual";

/// kotlinc's plugin reports every `linkToActual()` function before every `@Actual` function.
const EXPECT_PASS: crate::diag::DiagnosticKind = crate::diag::DiagnosticKind::IrPluginPass(0);
const ACTUAL_PASS: crate::diag::DiagnosticKind = crate::diag::DiagnosticKind::IrPluginPass(1);

fn multiplatform_package() -> TypeName {
    crate::types::type_name("fleet/util/multiplatform")
}

fn actual_annotation() -> TypeName {
    crate::types::type_name("fleet/util/multiplatform/Actual")
}

/// A parameter's IR kind, as the plugin's mismatch reasons name it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParameterKind {
    Context,
    ExtensionReceiver,
    Regular,
}

impl ParameterKind {
    fn ir_name(self) -> &'static str {
        match self {
            Self::Context => "Context",
            Self::ExtensionReceiver => "ExtensionReceiver",
            Self::Regular => "Regular",
        }
    }
}

struct ShapeParameter {
    name: String,
    ty: Ty,
    kind: ParameterKind,
}

struct ShapeTypeParameter {
    semantic: String,
    source: String,
    variance: crate::types::TypeVariance,
    bounds: Vec<Ty>,
}

/// The facts of one module function kotlinc's plugin compares, in IR parameter order: context
/// parameters, the extension receiver, then value parameters.
struct FunctionShape {
    fq_name: String,
    parameters: Vec<ShapeParameter>,
    type_parameters: Vec<ShapeTypeParameter>,
    result: Ty,
}

impl FunctionShape {
    fn of(index: &ResolvedModuleIndex, declaration: DeclarationId, fq_name: String) -> Option<Self> {
        let callable = index.callable_for_declaration(declaration)?;
        let signature = index.signature(declaration)?;
        let context = callable.shape.context_parameter_count as usize;
        let mut receiver = callable.shape.extension_receiver.map(|ty| ShapeParameter {
            name: "<this>".to_string(),
            ty: ty.get(),
            kind: ParameterKind::ExtensionReceiver,
        });
        let mut parameters = Vec::with_capacity(signature.parameters.len() + 1);
        for (ordinal, ty) in signature.parameters.iter().enumerate() {
            if ordinal == context {
                parameters.extend(receiver.take());
            }
            let ordinal_id = u32::try_from(ordinal).ok()?;
            let name = index
                .callable_parameter_name(callable.id, ordinal_id)
                .unwrap_or("_")
                .to_string();
            let vararg = index
                .callable_parameter(callable.id, ordinal_id)
                .is_some_and(|parameter| parameter.flags().is_vararg());
            parameters.push(ShapeParameter {
                name,
                ty: if vararg {
                    vararg_ir_type(ty.get())
                } else {
                    ty.get()
                },
                kind: if ordinal < context {
                    ParameterKind::Context
                } else {
                    ParameterKind::Regular
                },
            });
        }
        parameters.extend(receiver);
        let mut type_parameters = Vec::new();
        while let Some(parameter) =
            index.type_parameter(declaration, u32::try_from(type_parameters.len()).ok()?)
        {
            let header = index.type_parameter_header(parameter)?;
            let mut bounds = header
                .bounds
                .iter()
                .map(|bound| bound.ty.get())
                .collect::<Vec<_>>();
            if bounds.is_empty() {
                bounds.push(Ty::nullable(Ty::obj_name(crate::types::wk::any())));
            }
            type_parameters.push(ShapeTypeParameter {
                semantic: index.type_parameter_semantic_name(parameter)?.to_string(),
                source: index.type_parameter_name(parameter)?.to_string(),
                variance: header.flags.variance(),
                bounds,
            });
        }
        Some(Self {
            fq_name,
            parameters,
            type_parameters,
            result: signature.result.get(),
        })
    }

    /// kotlinc's `mismatchReason`: why `self` (`this`) does not match `to`, or `None`.
    fn mismatch_reason(&self, to: &FunctionShape) -> Option<String> {
        let renamed = self
            .type_parameters
            .iter()
            .zip(&to.type_parameters)
            .map(|(from, to)| (from.semantic.as_str(), to.semantic.as_str()))
            .collect::<HashMap<_, _>>();
        let renderer = TypeRenderer::of(&[self, to]);
        let same = |to: Ty, this: Ty| same_ir_type(to, this, &renamed, &renderer);
        if to.parameters.len() != self.parameters.len() {
            return Some(format!(
                "non matching number of parameters: this{} vs to{}",
                renderer.parameters(self),
                renderer.parameters(to)
            ));
        }
        if to.type_parameters.len() != self.type_parameters.len() {
            return Some("non matching number of type parameters".to_string());
        }
        if !same(to.result, self.result) {
            return Some(format!(
                "non matching return type: {} vs {}",
                renderer.ty(self.result),
                renderer.ty(to.result)
            ));
        }
        if !to
            .parameters
            .iter()
            .zip(&self.parameters)
            .all(|(expected, actual)| expected.kind == actual.kind && same(expected.ty, actual.ty))
        {
            return Some(format!(
                "non matching parameters: this{} vs to{}",
                renderer.parameters(self),
                renderer.parameters(to)
            ));
        }
        if !to
            .type_parameters
            .iter()
            .zip(&self.type_parameters)
            .all(|(expected, actual)| {
                expected.variance == actual.variance
                    && expected.bounds.len() == actual.bounds.len()
                    && expected
                        .bounds
                        .iter()
                        .zip(&actual.bounds)
                        .all(|(expected, actual)| same(*expected, *actual))
            })
        {
            return Some("non matching type parameters".to_string());
        }
        None
    }
}

/// A vararg parameter's IR type: `vararg x: String` is `Array<out String>`. A primitive vararg is
/// already its primitive array.
fn vararg_ir_type(ty: Ty) -> Ty {
    match ty {
        Ty::Obj(name, [element]) if name == crate::types::wk::array() && !matches!(element, Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_)) => {
            Ty::obj_args_name(name, &[Ty::out_projection(*element)])
        }
        ty => ty,
    }
}

/// kotlinc's IR type equality between a type of `to` and one of `this` whose type parameters are
/// renamed to `to`'s: the same classifier, nullability and arguments. Type annotations, such as a
/// function type's receiver marker, do not take part.
fn same_ir_type(
    to: Ty,
    this: Ty,
    renamed: &HashMap<&str, &str>,
    type_parameters: &TypeRenderer,
) -> bool {
    let (to, this) = (to.canonical_semantic(), this.canonical_semantic());
    let same = |to: Ty, this: Ty| same_ir_type(to, this, renamed, type_parameters);
    let all = |to: &[Ty], this: &[Ty]| {
        to.len() == this.len() && to.iter().zip(this).all(|(to, this)| same(*to, *this))
    };
    match (to, this) {
        (Ty::TyParam(to, to_bound), Ty::TyParam(this, this_bound)) => {
            renamed.get(this).copied().unwrap_or(this) == to
                && type_parameters.definitely_not_null(to, *to_bound)
                    == type_parameters.definitely_not_null(this, *this_bound)
        }
        (Ty::Obj(to_name, to_arguments), Ty::Obj(this_name, this_arguments)) => {
            to_name == this_name && all(to_arguments, this_arguments)
        }
        (Ty::Fun(to), Ty::Fun(this)) => {
            to.suspend == this.suspend && all(&to.params, &this.params) && same(to.ret, this.ret)
        }
        (Ty::Nullable(to), Ty::Nullable(this))
        | (Ty::PlatformNullable(to), Ty::PlatformNullable(this))
        | (Ty::DefinitelyNotNull(to), Ty::DefinitelyNotNull(this))
        | (Ty::InProjection(to), Ty::InProjection(this))
        | (Ty::OutProjection(to), Ty::OutProjection(this)) => same(*to, *this),
        (Ty::StarProjection(_), Ty::StarProjection(_)) => true,
        (Ty::Intersection(to), Ty::Intersection(this)) => all(to, this),
        (to, this) => to == this,
    }
}

/// Renders types as kotlinc's `IrType.render()` does in the plugin's messages: qualified
/// classifiers, `T of <owner>` type parameters and function types as their `FunctionN` classes.
struct TypeRenderer {
    /// Each type parameter's source name, owner, and whether its declared bound admits null.
    type_parameters: HashMap<String, (String, String, bool)>,
}

impl TypeRenderer {
    fn of(shapes: &[&FunctionShape]) -> Self {
        Self {
            type_parameters: shapes
                .iter()
                .flat_map(|shape| {
                    shape.type_parameters.iter().map(|parameter| {
                        let nullable = parameter
                            .bounds
                            .first()
                            .is_none_or(|bound| bound.upper_bound_admits_null());
                        (
                            parameter.semantic.clone(),
                            (parameter.source.clone(), shape.fq_name.clone(), nullable),
                        )
                    })
                })
                .collect(),
        }
    }

    /// Whether an occurrence of type parameter `name` carrying `bound` is `T & Any`: krusty
    /// records that type as the parameter with its bound made non-null.
    fn definitely_not_null(&self, name: &str, bound: Ty) -> bool {
        self.type_parameters
            .get(name)
            .is_some_and(|(_, _, nullable)| *nullable && !bound.upper_bound_admits_null())
    }

    fn parameters(&self, shape: &FunctionShape) -> String {
        let parameters = shape
            .parameters
            .iter()
            .map(|parameter| {
                format!(
                    "{}: {} [{}]",
                    parameter.name,
                    self.ty(parameter.ty),
                    parameter.kind.ir_name()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("({parameters})")
    }

    fn ty(&self, ty: Ty) -> String {
        let list = |types: &[Ty]| {
            types
                .iter()
                .map(|ty| self.ty(*ty))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match ty.canonical_semantic() {
            Ty::Unit => "kotlin.Unit".to_string(),
            Ty::Nothing => "kotlin.Nothing".to_string(),
            Ty::Null => "kotlin.Nothing?".to_string(),
            Ty::Obj(name, arguments) => {
                let name = name.render().replace(['/', '$'], ".");
                if arguments.is_empty() {
                    name
                } else {
                    format!("{name}<{}>", list(arguments))
                }
            }
            Ty::Fun(signature) => {
                let mut arguments = signature.params.clone();
                arguments.push(signature.ret);
                let class = if signature.suspend {
                    "kotlin.coroutines.SuspendFunction"
                } else {
                    "kotlin.Function"
                };
                let annotation = if signature.has_receiver {
                    "@[ExtensionFunctionType] "
                } else {
                    ""
                };
                format!(
                    "{annotation}{class}{}<{}>",
                    signature.params.len(),
                    list(&arguments)
                )
            }
            Ty::Nullable(inner) => format!("{}?", self.ty(*inner)),
            Ty::PlatformNullable(inner) => format!("{}!", self.ty(*inner)),
            Ty::DefinitelyNotNull(inner) => format!("{{{} & Any}}", self.ty(*inner)),
            Ty::InProjection(inner) => format!("in {}", self.ty(*inner)),
            Ty::OutProjection(inner) => format!("out {}", self.ty(*inner)),
            Ty::StarProjection(_) => "*".to_string(),
            Ty::Intersection(parts) => format!("{{{}}}", {
                parts
                    .iter()
                    .map(|part| self.ty(*part))
                    .collect::<Vec<_>>()
                    .join(" & ")
            }),
            Ty::TyParam(name, bound) => match self.type_parameters.get(name) {
                Some((source, owner, _)) if self.definitely_not_null(name, *bound) => {
                    format!("{{{source} of {owner} & Any}}")
                }
                Some((source, owner, _)) => format!("{source} of {owner}"),
                None => crate::types::type_parameter_source_name(name).to_string(),
            },
            Ty::Error | Ty::Pending => "<error>".to_string(),
        }
    }
}

/// A dotted Kotlin qualified name for `name` in `package`.
fn qualified(package: TypeName, name: &str) -> String {
    if package == TypeName::ROOT {
        name.to_string()
    } else {
        format!("{}.{name}", package.render().replace('/', "."))
    }
}

/// The module's top-level functions called `name` in `package`, in declaration order.
fn package_functions(
    index: &ResolvedModuleIndex,
    package: TypeName,
    name: &str,
) -> Vec<DeclarationId> {
    index
        .top_level_declarations_named(name)
        .iter()
        .copied()
        .filter(|&declaration| {
            index.declaration_header(declaration).is_some_and(|header| {
                header.kind == DeclarationKind::Function && header.owner.is_none()
            }) && index
                .declaration_anchor(declaration)
                .and_then(|anchor| index.source_package(anchor.source))
                .unwrap_or(TypeName::ROOT)
                == package
        })
        .collect()
}

impl Checker<'_> {
    /// Fleet's convention for one top-level function whose body this pass checked: link a
    /// `linkToActual()` function to its actual, or validate an `@Actual` function. Inline
    /// preparation records the link for the inline body it builds; only the authoritative pass
    /// reports.
    pub(super) fn link_expected_function(
        &mut self,
        f: &FunDecl,
        declaration: Option<DeclarationId>,
    ) {
        let Some(index) = self.resolved_index else {
            return;
        };
        let Some(suffix) = index.actual_function_suffix() else {
            return;
        };
        let Some(declaration) = declaration else {
            return;
        };
        if self.discover_anonymous_captures || self.inference_only {
            return;
        }
        let reports = matches!(self.fragment, SourceFragmentMode::Complete);
        if !reports && !matches!(self.fragment, SourceFragmentMode::InlinePreparation) {
            return;
        }
        let package = self.source_package;
        if self.calls_only_link_to_actual(f) {
            self.link_to_actual(index, suffix, package, f, declaration, reports);
        } else if reports
            && index
                .declaration_annotations(declaration)
                .contains(&actual_annotation())
        {
            self.check_actual_function(index, suffix, package, f, declaration);
        }
    }

    /// The same convention for a member function. kotlinc's plugin looks a member's counterpart up
    /// as a top-level function of a package named like its class, so a member never links.
    pub(super) fn check_member_expect_actual_convention(
        &mut self,
        f: &FunDecl,
        declaration: Option<DeclarationId>,
    ) {
        let Some(index) = self.resolved_index else {
            return;
        };
        let (Some(suffix), Some(declaration)) = (index.actual_function_suffix(), declaration)
        else {
            return;
        };
        if self.discover_anonymous_captures
            || self.inference_only
            || self.signature_defaults_only
            || !matches!(self.fragment, SourceFragmentMode::Complete)
            || !self
                .selected_body_declarations
                .as_ref()
                .is_none_or(|selected| selected.contains(&f.span))
        {
            return;
        }
        let Some(owner) = index
            .declaration_header(declaration)
            .and_then(|header| header.owner)
            .and_then(|owner| index.classifier_identity(owner))
        else {
            return;
        };
        let owner = owner.render().replace(['/', '$'], ".");
        if self.calls_only_link_to_actual(f) {
            self.diags.error_kind(
                f.signature_span,
                EXPECT_PASS,
                format!(
                    "no `@Actual fun {owner}.{}{suffix}()` found, cannot link `linkToActual()`",
                    f.name
                ),
            );
        } else if index
            .declaration_annotations(declaration)
            .contains(&actual_annotation())
        {
            let message = match f.name.strip_suffix(suffix) {
                Some(expect) => format!(
                    "no `fun {expect}()` calling {LINK_TO_ACTUAL}() found, invalid `@Actual` usage"
                ),
                None => format!(
                    "function {owner}.{} marked with @Actual should have a name that ending with `{suffix}`",
                    f.name
                ),
            };
            self.diags.error_kind(f.signature_span, ACTUAL_PASS, message);
        }
    }

    /// Whether `f`'s whole body is a call of `fleet.util.multiplatform.linkToActual`: an expression
    /// body, or a block whose only statement returns it.
    fn calls_only_link_to_actual(&self, f: &FunDecl) -> bool {
        let value = match &f.body {
            FunBody::Expr(expression) => Some(*expression),
            FunBody::Block(block) => match self.file.expr(*block) {
                Expr::Block { stmts, trailing } => match (stmts.as_slice(), trailing) {
                    ([statement], None) => match self.file.stmt(*statement) {
                        Stmt::Return(value, None) => *value,
                        Stmt::Expr(expression) => self.returned_value(*expression),
                        _ => None,
                    },
                    ([], Some(expression)) => self.returned_value(*expression),
                    _ => None,
                },
                _ => None,
            },
            FunBody::None => None,
        };
        let Some(value) = value else {
            return false;
        };
        let Some(ResolvedCall::TopLevel(call)) = self.resolved_calls.get(&value) else {
            return false;
        };
        if call.callable.name != LINK_TO_ACTUAL {
            return false;
        }
        match call.stable_declaration {
            Some(declaration) => self.resolved_index.is_some_and(|index| {
                index
                    .declaration_anchor(declaration)
                    .and_then(|anchor| index.source_package(anchor.source))
                    == Some(multiplatform_package())
            }),
            None => {
                call.callable.declaration_owner
                    == Some(crate::types::SemanticCallableOwner::Package(
                        multiplatform_package(),
                    ))
            }
        }
    }

    fn returned_value(&self, expression: ExprId) -> Option<ExprId> {
        match self.file.expr(expression) {
            Expr::Return { value, label: None } => *value,
            _ => None,
        }
    }

    fn link_to_actual(
        &mut self,
        index: &ResolvedModuleIndex,
        suffix: &str,
        package: TypeName,
        f: &FunDecl,
        declaration: DeclarationId,
        reports: bool,
    ) {
        let actual_name = format!("{}{suffix}", f.name);
        let actual_fq_name = qualified(package, &actual_name);
        let candidates = package_functions(index, package, &actual_name)
            .into_iter()
            .filter(|&candidate| {
                index
                    .declaration_annotations(candidate)
                    .contains(&actual_annotation())
            })
            .collect::<Vec<_>>();
        let dependency_candidates = self.dependency_declares_function(package, &actual_name);
        let Some(expect) = FunctionShape::of(index, declaration, qualified(package, &f.name)) else {
            return;
        };
        let shapes = candidates
            .iter()
            .filter_map(|&candidate| {
                Some((
                    candidate,
                    FunctionShape::of(index, candidate, actual_fq_name.clone())?,
                ))
            })
            .collect::<Vec<_>>();
        if let Some(&(actual, _)) = shapes
            .iter()
            .find(|(_, shape)| shape.mismatch_reason(&expect).is_none())
        {
            if let Some(message) = self.unlinkable_parameters(f) {
                if reports {
                    self.diags.error_kind(f.signature_span, EXPECT_PASS, message);
                }
                return;
            }
            self.linked_actuals.insert(declaration, actual);
            return;
        }
        if !reports {
            return;
        }
        let message = if dependency_candidates {
            format!(
                "krusty links `{LINK_TO_ACTUAL}()` only to an `@Actual` function of the module being \
                 compiled: it cannot read whether a dependency's `{actual_fq_name}()` is `@Actual`"
            )
        } else if shapes.is_empty() {
            format!("no `@Actual fun {actual_fq_name}()` found, cannot link `{LINK_TO_ACTUAL}()`")
        } else {
            let reasons = shapes
                .iter()
                .filter_map(|(_, shape)| {
                    Some(format!(
                        "{}: {}",
                        shape.fq_name,
                        shape.mismatch_reason(&expect)?
                    ))
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "none of existing `@Actual fun {actual_fq_name}()` have compatible signature \
                 [{reasons}], cannot link `{LINK_TO_ACTUAL}()`"
            )
        };
        self.diags.error_kind(f.signature_span, EXPECT_PASS, message);
    }

    /// Why krusty cannot pass every parameter of `f` on to its actual: an unnamed context
    /// parameter is a receiver with no value of its own to read.
    fn unlinkable_parameters(&self, f: &FunDecl) -> Option<String> {
        f.params
            .iter()
            .take(f.context_count)
            .any(|parameter| parameter.context_kind != crate::types::ContextParameterKind::Named)
            .then(|| {
                format!(
                    "krusty cannot link `{LINK_TO_ACTUAL}()` in `{}`: it declares a context \
                     parameter without a name",
                    f.name
                )
            })
    }

    fn check_actual_function(
        &mut self,
        index: &ResolvedModuleIndex,
        suffix: &str,
        package: TypeName,
        f: &FunDecl,
        declaration: DeclarationId,
    ) {
        let fq_name = qualified(package, &f.name);
        let Some(expect_name) = f.name.strip_suffix(suffix) else {
            self.diags.error_kind(
                f.signature_span,
                ACTUAL_PASS,
                format!(
                    "function {fq_name} marked with @Actual should have a name that ending with `{suffix}`"
                ),
            );
            return;
        };
        let expect_fq_name = qualified(package, expect_name);
        let Some(actual) = FunctionShape::of(index, declaration, fq_name) else {
            return;
        };
        let expects = package_functions(index, package, expect_name)
            .into_iter()
            .filter_map(|candidate| FunctionShape::of(index, candidate, expect_fq_name.clone()))
            .collect::<Vec<_>>();
        if expects
            .iter()
            .any(|expect| expect.mismatch_reason(&actual).is_none())
        {
            return;
        }
        let message = if self.dependency_declares_function(package, expect_name) {
            format!(
                "krusty checks an `@Actual` function only against functions of the module being \
                 compiled: it cannot compare `{}` with a dependency's `{expect_fq_name}()`",
                actual.fq_name
            )
        } else if expects.is_empty() {
            format!(
                "no `fun {expect_name}()` calling {LINK_TO_ACTUAL}() found, invalid `@Actual` usage"
            )
        } else {
            let reasons = expects
                .iter()
                .filter_map(|expect| {
                    Some(format!(
                        "{}: {}",
                        expect.fq_name,
                        expect.mismatch_reason(&actual)?
                    ))
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "none of existing `fun {expect_name}()` has compatible signature [{reasons}], \
                 invalid `@Actual` usage"
            )
        };
        self.diags.error_kind(f.signature_span, ACTUAL_PASS, message);
    }

    /// Whether a dependency declares a top-level function `name` in `package`.
    fn dependency_declares_function(&self, package: TypeName, name: &str) -> bool {
        !self
            .libraries
            .symbols(crate::symbol_source::SymbolNamespace::Package(package), name)
            .callables
            .functions()
            .is_empty()
    }
}
