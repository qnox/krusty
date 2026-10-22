//! The declarations KSP lists for a classifier, and the functions, properties and constructors
//! among them, in KSP's order.

use krusty_plugin_ir::{
    KspClass, KspClassKind, KspDeclaration, KspFunction, KspFunctionKind, KspModifier,
    KspParameter, KspProperty, KspType, KspTypeArgument, KspTypeParameter, KspTypeSlot,
};

use super::annotations::Placement;
use super::type_model::{class_type, TypeParameterScope};
use super::{Owner, Source, Subject};
use crate::ast::{
    AstEnumEntry, ClassDecl, Decl, DeclId, FunBody, FunDecl, Param, PropDecl, PropParam,
    SecondaryCtor,
};
use crate::frontend::{FrontendClassSig, FrontendDeclarationId};
use crate::types::{Ty, Visibility};

/// What a classifier's declarations are built from.
pub(super) struct ClassHeader<'a> {
    pub(super) id: DeclId,
    pub(super) class: &'a ClassDecl,
    pub(super) signature: &'a FrontendClassSig,
    pub(super) kind: KspClassKind,
    pub(super) qualified_name: &'a str,
    pub(super) type_parameters: &'a Option<Vec<KspTypeParameter>>,
    pub(super) scope: &'a TypeParameterScope,
}

impl ClassHeader<'_> {
    /// The classifier applied to a star for each of its type parameters, which is what KSP
    /// reports a constructor returns.
    fn star_projected(&self) -> KspType {
        let mut ty = class_type(self.qualified_name);
        ty.arguments = vec![KspTypeArgument::Star; self.class.type_parameters.type_params().len()];
        ty
    }

    /// The resolved type of the primary constructor's parameter at `index`.
    fn constructor_parameter_type(&self, index: usize) -> Option<KspType> {
        let (ty, _) = *self.signature.ctor_param_shapes.get(index)?;
        self.scope.ty(declared_parameter_type(
            ty,
            self.class.props.get(index)?.is_vararg,
        )?)
    }
}

impl Source<'_> {
    /// The classifier's declarations in KSP's order, and whether they are all it lists.
    pub(super) fn class_declarations(
        &self,
        header: &ClassHeader<'_>,
    ) -> Result<(Vec<KspDeclaration>, bool), String> {
        let class = header.class;
        // A nested type alias and a member of a companion block are not described yet.
        let complete = class.type_aliases.is_empty() && class.companion_block_members.is_empty();
        let mut declarations = Vec::new();
        for (index, parameter) in class.props.iter().enumerate() {
            if parameter.is_property {
                declarations.push(KspDeclaration::Property(
                    self.constructor_property(index, parameter, header)?,
                ));
            }
        }
        let mut members: Vec<(u32, KspDeclaration)> = Vec::new();
        for (method, function) in class.methods.iter().enumerate() {
            let owner = Owner::Class {
                header,
                method: Some(method as u32),
            };
            members.push((
                function.span.lo,
                KspDeclaration::Function(self.function(function, &owner, header.scope)?),
            ));
        }
        for property in &class.body_props {
            let owner = Owner::Class {
                header,
                method: None,
            };
            members.push((
                property.span.lo,
                KspDeclaration::Property(self.property(property, &owner, header.scope)?),
            ));
        }
        members.sort_by_key(|(start, _)| *start);
        declarations.extend(members.into_iter().map(|(_, declaration)| declaration));
        if class.is_data && header.kind == KspClassKind::Class {
            declarations.extend(self.data_members(header));
        }
        if let Some(primary) = self.primary_constructor(header)? {
            declarations.push(KspDeclaration::Function(primary));
        }
        for (index, constructor) in class.secondary_ctors.iter().enumerate() {
            declarations.push(KspDeclaration::Function(self.secondary_constructor(
                index,
                constructor,
                header,
            )?));
        }
        for entry in &class.enum_entries {
            declarations.push(KspDeclaration::Class(self.enum_entry(entry, header)?));
        }
        for &nested in &self.file.decls {
            if !self.is_member_of(nested, Some(header.id)) {
                continue;
            }
            if let Decl::Class(nested_class) = self.file.decl(nested) {
                declarations.push(KspDeclaration::Class(self.class(
                    nested,
                    nested_class,
                    header.scope,
                )?));
            }
        }
        if header.kind == KspClassKind::EnumClass {
            declarations.extend(self.enum_members(header));
        }
        Ok((declarations, complete))
    }

    pub(super) fn function(
        &self,
        function: &FunDecl,
        owner: &Owner<'_>,
        enclosing: &TypeParameterScope,
    ) -> Result<KspFunction, String> {
        let subject = Subject {
            kind: "function",
            name: &function.name,
            line: function.decl_line,
        };
        let qualified_name = owner.qualified_name(&function.name);
        let signature = match owner {
            Owner::TopLevel { id, .. } => self.resolved.functions.get(&(self.index, *id)),
            Owner::Class { header, method } => method.and_then(|method| {
                self.resolved
                    .methods
                    .get(&(header.signature.stable_declaration?, method))
            }),
        };
        let generic = signature.and_then(|signature| signature.generic_sig.as_ref());
        let formals = generic.map_or(&[][..], |generic| &generic.formals);
        let scope = enclosing.with(
            self.ids,
            &qualified_name,
            formals
                .iter()
                .zip(&function.type_params)
                .map(|(identity, name)| (identity.as_str(), name.as_str())),
        );
        let type_parameters = (function.type_params.len() == formals.len())
            .then(|| {
                let generic = generic?;
                formals
                    .iter()
                    .zip(&generic.formal_bounds)
                    .zip(&function.type_params)
                    .map(|((identity, bounds), name)| {
                        scope.type_parameter(
                            identity,
                            crate::types::TypeVariance::Invariant,
                            function.reified_type_params.contains(name),
                            bounds_or_nullable_any(bounds),
                        )
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .flatten()
            .or_else(|| function.type_params.is_empty().then(Vec::new));
        let return_type = signature
            .and_then(|signature| scope.ty(generic.map_or(signature.ret, |generic| generic.ret)));
        // The resolved parameter types, parallel to the written parameters.
        let parameter_types = signature
            .map(|signature| generic.map_or(&signature.params, |generic| &generic.params))
            .filter(|types| types.len() == function.params.len());
        let receiver = function.receiver.as_ref().map(|_| {
            signature.and_then(|signature| {
                generic
                    .and_then(|generic| generic.receiver)
                    .or(signature.source_receiver)
            })
        });
        let is_member = matches!(owner, Owner::Class { .. });
        let declaration = signature.and_then(|signature| signature.stable_declaration);
        let mut parameters = Vec::with_capacity(function.params.len());
        for (index, parameter) in function.params.iter().enumerate().skip(function.context_count) {
            let resolved = parameter_types.map(|types| types[index]);
            let mut described = parameter_description(parameter, resolved, &scope);
            // A member function's parameter also has the default a function it overrides
            // declares.
            if is_member && described.has_default == Some(false) {
                if let Some(declaration) = declaration {
                    described.has_default = self.member_parameter_default(declaration, index);
                }
            }
            described.annotations = self.published_annotations(
                subject,
                &parameter.annotations,
                declaration.map(|declaration| {
                    self.analysis
                        .parameter_annotations(declaration, index as u32)
                }),
                Placement::All,
            )?;
            parameters.push(described);
        }
        Ok(KspFunction {
            name: function.name.clone(),
            kind: if is_member {
                KspFunctionKind::Member
            } else {
                KspFunctionKind::TopLevel
            },
            is_abstract: is_member && matches!(function.body, FunBody::None),
            line: Some(function.decl_line),
            annotations: self.published_annotations(
                subject,
                &function.annotations,
                declaration.map(|declaration| self.analysis.declaration_annotations(declaration)),
                Placement::All,
            )?,
            modifiers: self.written_modifiers(function.declaration_span.lo),
            type_parameters,
            extension_receiver: receiver_slot(receiver, &scope),
            parameters,
            return_type,
        })
    }

    /// Whether the member function `declaration` declares or inherits a default for its parameter
    /// at `index`, or `None` when a function it overrides is a library's.
    fn member_parameter_default(
        &self,
        declaration: FrontendDeclarationId,
        index: usize,
    ) -> Option<bool> {
        let signature = self.resolved.member_functions.get(&declaration)?;
        if signature.param_defaults.as_slice().get(index) == Some(&true) {
            return Some(true);
        }
        let (owner, _) = self.analysis.member_function_slot(declaration)?;
        let mut inherited = Some(false);
        for overridden in self.analysis.overridden_functions(owner, declaration) {
            match overridden.and_then(|overridden| self.member_parameter_default(overridden, index))
            {
                Some(true) => return Some(true),
                Some(false) => {}
                None => inherited = None,
            }
        }
        inherited
    }

    pub(super) fn property(
        &self,
        property: &PropDecl,
        owner: &Owner<'_>,
        scope: &TypeParameterScope,
    ) -> Result<KspProperty, String> {
        let subject = Subject {
            kind: "property",
            name: &property.name,
            line: property.decl_line,
        };
        // The extension receiver it writes, resolved or not, and its type.
        let (receiver, ty) = match owner {
            Owner::TopLevel { id, .. } if property.receiver.is_some() => {
                match self.symbols.source_extension_property((self.index, id.0)) {
                    Some(resolved) => (Some(Some(resolved.receiver)), scope.ty(resolved.ty)),
                    None => (Some(None), None),
                }
            }
            Owner::TopLevel { id, .. } => (
                None,
                self.symbols
                    .source_props
                    .get(&(self.index, id.0))
                    .and_then(|resolved| scope.ty(resolved.ty)),
            ),
            // A member extension property is not described yet.
            Owner::Class { .. } if property.receiver.is_some() => (Some(None), None),
            Owner::Class { header, .. } => (
                None,
                header
                    .signature
                    .declared_props
                    .get(&property.name)
                    .and_then(|resolved| scope.ty(resolved.ty)),
            ),
        };
        let declaration = match owner {
            Owner::TopLevel { id, .. } => self
                .file
                .decls
                .iter()
                .position(|declared| declared == id)
                .and_then(|position| {
                    self.analysis
                        .top_level_property_declaration(self.index, position as u32)
                }),
            Owner::Class { header, .. } => header
                .class
                .body_props
                .iter()
                .position(|declared| std::ptr::eq(declared, property))
                .zip(header.signature.stable_declaration)
                .and_then(|(position, class)| {
                    self.analysis
                        .member_property_declaration(class, position as u32, false)
                }),
        };
        let modifiers = self.written_modifiers(property.declaration_span.lo);
        let (getter, setter) =
            self.written_accessors(subject, property, declaration, modifiers.as_deref())?;
        Ok(KspProperty {
            name: property.name.clone(),
            is_mutable: property.is_var,
            line: Some(property.decl_line),
            annotations: self.published_annotations(
                subject,
                &property.annotations,
                declaration.map(|declaration| self.analysis.declaration_annotations(declaration)),
                Placement::Property,
            )?,
            modifiers,
            // A generic property's type parameters are not described yet.
            type_parameters: property.type_params.is_empty().then(Vec::new),
            extension_receiver: receiver_slot(receiver, scope),
            ty,
            getter,
            setter,
        })
    }

    /// The property a `val` or `var` primary-constructor parameter declares.
    fn constructor_property(
        &self,
        index: usize,
        parameter: &PropParam,
        header: &ClassHeader<'_>,
    ) -> Result<KspProperty, String> {
        let subject = Subject {
            kind: "property",
            name: &parameter.name,
            line: parameter.decl_line,
        };
        let modifiers = self
            .written_modifiers(parameter.declaration_span.lo)
            .map(|modifiers| {
                modifiers
                    .into_iter()
                    .filter(|modifier| *modifier != KspModifier::Vararg)
                    .collect::<Vec<_>>()
            });
        let class = header.signature.stable_declaration;
        let written = class
            .and_then(|class| self.analysis.constructor_declaration(class, 0))
            .map(|constructor| {
                self.analysis
                    .parameter_annotations(constructor, index as u32)
            });
        let declaration = class.and_then(|class| {
            self.analysis
                .member_property_declaration(class, index as u32, true)
        });
        let (getter, setter) = self.constructor_accessors(
            subject,
            parameter,
            declaration,
            written,
            modifiers.as_deref(),
        )?;
        Ok(KspProperty {
            name: parameter.name.clone(),
            is_mutable: parameter.is_var,
            line: Some(parameter.decl_line),
            annotations: self.published_annotations(
                subject,
                &parameter.annotations,
                written,
                Placement::Property,
            )?,
            modifiers,
            type_parameters: Some(Vec::new()),
            extension_receiver: KspTypeSlot::Absent,
            ty: header.constructor_parameter_type(index),
            getter,
            setter,
        })
    }

    /// The primary constructor the resolved declaration has: the one written, or the implicit one
    /// of a class without secondary constructors. An interface has no constructor.
    fn primary_constructor(&self, header: &ClassHeader<'_>) -> Result<Option<KspFunction>, String> {
        let (class, signature) = (header.class, header.signature);
        if header.kind == KspClassKind::Interface || !signature.has_primary_ctor {
            return Ok(None);
        }
        // The signature names each parameter and whether it has a default; the declaration says
        // how each was written.
        if signature.ctor_param_names.len() != class.props.len() {
            return Err(self.refusal(
                Subject {
                    kind: "class",
                    name: &class.name,
                    line: class.decl_line,
                },
                &format!(
                    "has {} primary-constructor parameters resolved for {} written",
                    signature.ctor_param_names.len(),
                    class.props.len()
                ),
            ));
        }
        let subject = Subject {
            kind: "constructor of",
            name: &class.name,
            line: class.primary_ctor_lines.decl_line,
        };
        let declaration = signature
            .stable_declaration
            .and_then(|class| self.analysis.constructor_declaration(class, 0));
        let mut parameters = Vec::with_capacity(class.props.len());
        for (index, ((name, has_default), written)) in
            signature.ctor_param_names.iter().zip(&class.props).enumerate()
        {
            parameters.push(KspParameter {
                name: name.clone(),
                ty: header.constructor_parameter_type(index),
                is_val: written.is_property && !written.is_var,
                is_var: written.is_property && written.is_var,
                is_vararg: written.is_vararg,
                is_noinline: false,
                is_crossinline: false,
                has_default: Some(*has_default),
                annotations: self.published_annotations(
                    subject,
                    &written.annotations,
                    declaration.map(|declaration| {
                        self.analysis
                            .parameter_annotations(declaration, index as u32)
                    }),
                    Placement::Parameter,
                )?,
            });
        }
        let written = class.primary_ctor_lines.decl_line != 0;
        let visibility = class
            .primary_ctor_visibility_span
            .map(|_| class.primary_ctor_visibility);
        let annotations = self.published_annotations(
            subject,
            class.primary_ctor_annotations.iter().flatten(),
            declaration.map(|declaration| self.analysis.declaration_annotations(declaration)),
            Placement::All,
        )?;
        Ok(Some(constructor(
            header,
            KspFunctionKind::PrimaryConstructor,
            written.then_some(class.primary_ctor_lines.decl_line),
            annotations,
            Some(constructor_modifiers(header.kind, written, visibility)),
            parameters,
        )))
    }

    fn secondary_constructor(
        &self,
        index: usize,
        constructor_decl: &SecondaryCtor,
        header: &ClassHeader<'_>,
    ) -> Result<KspFunction, String> {
        let subject = Subject {
            kind: "constructor of",
            name: &header.class.name,
            line: constructor_decl.decl_line,
        };
        let modifiers = self
            .file
            .declaration_prefixes
            .at(constructor_decl.declaration_span.lo)
            .map(|prefix| constructor_modifiers(header.kind, true, prefix.visibility));
        let resolved = header
            .signature
            .secondary_ctor_shapes
            .get(index)
            .filter(|types| types.len() == constructor_decl.params.len());
        let declaration = header
            .signature
            .stable_declaration
            .and_then(|class| self.analysis.constructor_declaration(class, index as u32 + 1));
        let mut parameters = Vec::with_capacity(constructor_decl.params.len());
        for (position, parameter) in constructor_decl.params.iter().enumerate() {
            let resolved = resolved.map(|types| types[position]);
            let mut described = parameter_description(parameter, resolved, header.scope);
            described.annotations = self.published_annotations(
                subject,
                &parameter.annotations,
                declaration.map(|declaration| {
                    self.analysis
                        .parameter_annotations(declaration, position as u32)
                }),
                Placement::All,
            )?;
            parameters.push(described);
        }
        Ok(constructor(
            header,
            KspFunctionKind::SecondaryConstructor,
            Some(constructor_decl.decl_line),
            self.published_annotations(
                subject,
                &constructor_decl.annotations,
                declaration.map(|declaration| self.analysis.declaration_annotations(declaration)),
                Placement::All,
            )?,
            modifiers,
            parameters,
        ))
    }

    fn enum_entry(
        &self,
        entry: &AstEnumEntry,
        header: &ClassHeader<'_>,
    ) -> Result<KspClass, String> {
        let subject = Subject {
            kind: "enum entry",
            name: &entry.name,
            line: entry.decl_line,
        };
        let qualified_name = format!("{}.{}", header.qualified_name, entry.name);
        Ok(KspClass {
            simple_name: entry.name.clone(),
            qualified_name,
            kind: KspClassKind::EnumEntry,
            line: entry.decl_line,
            annotations: self.annotations(subject, &entry.annotations)?,
            modifiers: Some(Vec::new()),
            type_parameters: Some(Vec::new()),
            super_types: Some(Vec::new()),
            declarations: Vec::new(),
            // An entry's own body is not described yet.
            declarations_complete: entry.methods.is_empty()
                && entry.props.is_empty()
                && !self
                    .file
                    .enum_entry_nested_classifier_owners
                    .values()
                    .any(|body| *body == entry.span),
        })
    }

    /// A data class's `componentN` for each primary-constructor property, then `copy`.
    fn data_members(&self, header: &ClassHeader<'_>) -> Vec<KspDeclaration> {
        let properties: Vec<(usize, &PropParam)> = header
            .class
            .props
            .iter()
            .enumerate()
            .filter(|(_, parameter)| parameter.is_property)
            .collect();
        // `copy` has the visibility the resolved declaration the compiler generated has: public, or
        // the primary constructor's where the language or `@ConsistentCopyVisibility` says so.
        let copy_visibility = header
            .signature
            .methods
            .get("copy")
            .into_iter()
            .flatten()
            .find(|copy| {
                copy.stable_declaration
                    .is_some_and(|declaration| self.analysis.is_compiler_generated(declaration))
            })
            .map_or(Visibility::Public, |copy| copy.visibility);
        let mut copy_modifiers = vec![KspModifier::Final];
        copy_modifiers.extend(visibility_modifier(copy_visibility));
        let mut members: Vec<KspDeclaration> = properties
            .iter()
            .enumerate()
            .map(|(component, (index, _))| {
                KspDeclaration::Function(synthetic_function(
                    format!("component{}", component + 1),
                    KspFunctionKind::Member,
                    vec![
                        KspModifier::Final,
                        KspModifier::Operator,
                        KspModifier::Override,
                        KspModifier::Public,
                    ],
                    Vec::new(),
                    header.constructor_parameter_type(*index),
                ))
            })
            .collect();
        let mut owner = class_type(header.qualified_name);
        owner.arguments = header
            .type_parameters
            .iter()
            .flatten()
            .map(|parameter| {
                KspTypeArgument::Invariant(KspType {
                    declaration: krusty_plugin_ir::KspTypeDeclaration::TypeParameter {
                        id: parameter.id,
                        owner: header.qualified_name.to_string(),
                        name: parameter.name.clone(),
                    },
                    nullable: false,
                    arguments: Vec::new(),
                })
            })
            .collect();
        members.push(KspDeclaration::Function(synthetic_function(
            "copy".to_string(),
            KspFunctionKind::Member,
            copy_modifiers,
            properties
                .iter()
                .map(|(index, property)| KspParameter {
                    name: property.name.clone(),
                    ty: header.constructor_parameter_type(*index),
                    is_val: false,
                    is_var: false,
                    is_vararg: false,
                    is_noinline: false,
                    is_crossinline: false,
                    has_default: Some(true),
                    annotations: Vec::new(),
                })
                .collect(),
            Some(owner),
        )));
        members
    }

    /// An enum class's `values`, `valueOf` and, where the language has it, `entries`.
    fn enum_members(&self, header: &ClassHeader<'_>) -> Vec<KspDeclaration> {
        let modifiers = vec![
            KspModifier::Final,
            KspModifier::JavaStatic,
            KspModifier::Public,
        ];
        let enum_type = class_type(header.qualified_name);
        let applied = |classifier: &str| KspType {
            arguments: vec![KspTypeArgument::Invariant(enum_type.clone())],
            ..class_type(classifier)
        };
        let mut members = vec![
            KspDeclaration::Function(synthetic_function(
                "values".to_string(),
                KspFunctionKind::Static,
                modifiers.clone(),
                Vec::new(),
                Some(applied("kotlin.Array")),
            )),
            KspDeclaration::Function(synthetic_function(
                "valueOf".to_string(),
                KspFunctionKind::Static,
                modifiers.clone(),
                vec![KspParameter {
                    name: "value".to_string(),
                    ty: Some(class_type("kotlin.String")),
                    is_val: false,
                    is_var: false,
                    is_vararg: false,
                    is_noinline: false,
                    is_crossinline: false,
                    has_default: Some(false),
                    annotations: Vec::new(),
                }],
                Some(enum_type.clone()),
            )),
        ];
        if self.file.enum_entries_enabled {
            members.push(KspDeclaration::Property(KspProperty {
                name: "entries".to_string(),
                is_mutable: false,
                line: None,
                annotations: Vec::new(),
                modifiers: Some(modifiers),
                type_parameters: Some(Vec::new()),
                extension_receiver: KspTypeSlot::Absent,
                ty: Some(applied("kotlin.enums.EnumEntries")),
                getter: super::accessors::compiler_getter(),
                setter: None,
            }));
        }
        members
    }
}

/// A written parameter described with its `resolved` type.
fn parameter_description(
    parameter: &Param,
    resolved: Option<Ty>,
    scope: &TypeParameterScope,
) -> KspParameter {
    KspParameter {
        name: parameter.name.clone(),
        ty: resolved
            .and_then(|ty| declared_parameter_type(ty, parameter.is_vararg))
            .and_then(|ty| scope.ty(ty)),
        is_val: false,
        is_var: false,
        is_vararg: parameter.is_vararg,
        is_noinline: parameter.is_materialized_lambda,
        is_crossinline: parameter.is_crossinline,
        has_default: Some(parameter.default.is_some()),
        annotations: Vec::new(),
    }
}

/// A parameter's type as KSP reports it from the resolved one: a vararg's is its element type.
fn declared_parameter_type(resolved: Ty, is_vararg: bool) -> Option<Ty> {
    if is_vararg {
        resolved.array_read_elem()
    } else {
        Some(resolved)
    }
}

/// The extension receiver slot of a declaration that writes `receiver`, resolved or not.
fn receiver_slot(receiver: Option<Option<Ty>>, scope: &TypeParameterScope) -> KspTypeSlot {
    match receiver {
        None => KspTypeSlot::Absent,
        Some(resolved) => resolved
            .and_then(|ty| scope.ty(ty))
            .map_or(KspTypeSlot::Undescribed, KspTypeSlot::Present),
    }
}

/// A declared type parameter's bounds, or Kotlin's implicit `Any?` when it writes none.
fn bounds_or_nullable_any(bounds: &[Ty]) -> Vec<Ty> {
    if bounds.is_empty() {
        vec![Ty::nullable(Ty::obj("kotlin/Any"))]
    } else {
        bounds.to_vec()
    }
}

/// What KSP reports a constructor's modifiers are: `final`, the visibility written or, for an
/// implicit constructor, `public`; an enum class's constructor is always `private`.
fn constructor_modifiers(
    kind: KspClassKind,
    written: bool,
    visibility: Option<Visibility>,
) -> Vec<KspModifier> {
    let mut modifiers = vec![KspModifier::Final];
    let visibility = match (kind, visibility) {
        (KspClassKind::EnumClass, _) => Some(Visibility::Private),
        (_, Some(visibility)) => Some(visibility),
        (_, None) if !written => Some(Visibility::Public),
        (_, None) => None,
    };
    modifiers.extend(visibility.and_then(visibility_modifier));
    modifiers
}

/// The modifier KSP reports for a Kotlin visibility.
fn visibility_modifier(visibility: Visibility) -> Option<KspModifier> {
    match visibility {
        Visibility::Public => Some(KspModifier::Public),
        Visibility::Internal => Some(KspModifier::Internal),
        Visibility::Protected => Some(KspModifier::Protected),
        Visibility::Private => Some(KspModifier::Private),
        Visibility::PackagePrivate => None,
    }
}

fn constructor(
    header: &ClassHeader<'_>,
    kind: KspFunctionKind,
    line: Option<u32>,
    annotations: Vec<krusty_plugin_ir::KspAnnotation>,
    modifiers: Option<Vec<KspModifier>>,
    parameters: Vec<KspParameter>,
) -> KspFunction {
    KspFunction {
        name: "<init>".to_string(),
        kind,
        is_abstract: false,
        line,
        annotations,
        modifiers,
        type_parameters: header.type_parameters.clone(),
        extension_receiver: KspTypeSlot::Absent,
        parameters,
        return_type: Some(header.star_projected()),
    }
}

/// A function the language contributes to a classifier.
fn synthetic_function(
    name: String,
    kind: KspFunctionKind,
    modifiers: Vec<KspModifier>,
    parameters: Vec<KspParameter>,
    return_type: Option<KspType>,
) -> KspFunction {
    KspFunction {
        name,
        kind,
        is_abstract: false,
        line: None,
        annotations: Vec::new(),
        modifiers: Some(modifiers),
        type_parameters: Some(Vec::new()),
        extension_receiver: KspTypeSlot::Absent,
        parameters,
        return_type,
    }
}
