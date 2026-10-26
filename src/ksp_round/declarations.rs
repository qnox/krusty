//! The declarations KSP lists for a classifier, and the functions, properties and constructors
//! among them, in KSP's order.

use krusty_plugin_ir::{
    KspClass, KspClassKind, KspDeclaration, KspDeclarationOrigin, KspFunction, KspFunctionKind,
    KspModifier, KspParameter, KspProperty, KspType, KspTypeArgument, KspTypeParameter,
    KspTypeSlot,
};

use super::annotations::Placement;
use super::platform_names::{FunctionNaming, NamingSite};
use super::type_model::{as_written, class_type, TypeParameterScope};
use super::{Owner, Source, Subject};
use crate::ast::{
    AstEnumEntry, ClassDecl, Decl, DeclId, File, FunBody, FunDecl, Param, PropDecl, PropParam,
    SecondaryCtor, TypeRef,
};
use crate::frontend::{
    FrontendClassSig, FrontendDeclarationId, FrontendDeclaredSpellings as DeclaredSpellings,
    FrontendSpelled as Spelled,
};
use crate::types::{wk, ResolvedAnnotation, Ty, Visibility};

/// What a classifier's declarations are built from.
pub(super) struct ClassHeader<'a> {
    pub(super) id: DeclId,
    pub(super) file: &'a File,
    pub(super) class: &'a ClassDecl,
    pub(super) signature: &'a FrontendClassSig,
    pub(super) kind: KspClassKind,
    pub(super) qualified_name: &'a str,
    pub(super) type_parameters: &'a Option<Vec<KspTypeParameter>>,
    pub(super) scope: &'a TypeParameterScope,
    /// The resolved spellings of the primary constructor's parameter types.
    pub(super) primary_spellings: Option<&'a DeclaredSpellings>,
}

impl ClassHeader<'_> {
    /// Whether the class names its properties' accessors as the properties: an annotation class
    /// or a `@JvmRecord` class does.
    fn bare_accessors(&self) -> bool {
        self.kind == KspClassKind::AnnotationClass
            || self
                .signature
                .applied_annotations
                .iter()
                .any(|annotation| annotation.annotation == wk::jvm_record())
    }

    /// The classifier applied to a star for each of its type parameters, which is what KSP
    /// reports a constructor returns, with no annotations.
    fn star_projected(&self) -> KspType {
        let mut ty = class_type(self.qualified_name);
        ty.arguments = vec![KspTypeArgument::Star; self.class.type_parameters.type_params().len()];
        ty.unannotated = true;
        ty
    }

    /// The type of the property primary-constructor parameter `index` declares: the resolved
    /// slot, which for a `vararg` parameter is its array, `Array<out T>` for a reference element.
    fn constructor_property_type(&self, index: usize) -> Option<KspType> {
        let ty = self.scope.ty(self.constructor_property(index)?)?;
        // A `vararg` property's array is not what its parameter spells.
        let property = self.class.props.get(index)?;
        match property.is_vararg {
            true => Some(ty),
            false => as_written(self.file, &property.ty, self.primary_spelled(index), ty),
        }
    }

    /// [`Self::constructor_property_type`] as the resolved type.
    fn constructor_property(&self, index: usize) -> Option<Ty> {
        let (ty, _) = *self.signature.ctor_param_shapes.get(index)?;
        Some(
            if self.class.props.get(index)?.is_vararg && ty.is_reference_array() {
                Ty::obj_args_name(
                    crate::types::wk::array(),
                    &[Ty::out_projection(ty.array_read_elem()?)],
                )
            } else {
                ty
            },
        )
    }

    /// The resolved type of the primary constructor's parameter at `index`.
    fn constructor_parameter_type(&self, index: usize) -> Option<KspType> {
        let ty = self.scope.ty(self.constructor_parameter(index)?)?;
        as_written(
            self.file,
            &self.class.props.get(index)?.ty,
            self.primary_spelled(index),
            ty,
        )
    }

    /// The resolved spelling of the primary constructor's parameter at `index`.
    fn primary_spelled(&self, index: usize) -> Option<&Spelled> {
        self.primary_spellings
            .map(|spellings| spellings.param(index))
    }

    /// [`Self::constructor_parameter_type`] as the resolved type.
    fn constructor_parameter(&self, index: usize) -> Option<Ty> {
        let (ty, _) = *self.signature.ctor_param_shapes.get(index)?;
        declared_parameter_type(ty, self.class.props.get(index)?.is_vararg)
    }

    /// The classifier applied to its own type parameters.
    fn class_ty(&self) -> Ty {
        Ty::obj_name(self.signature.internal)
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
                declarations.push(KspDeclaration::Property(Box::new(
                    self.constructor_property(index, parameter, header)?,
                )));
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
                KspDeclaration::Property(Box::new(self.property(
                    property,
                    &owner,
                    header.scope,
                )?)),
            ));
        }
        members.sort_by_key(|(start, _)| *start);
        declarations.extend(members.into_iter().map(|(_, declaration)| declaration));
        if class.is_data && header.kind == KspClassKind::Class {
            declarations.extend(self.data_members(header));
        }
        // KSP lists inner classes with the members, before the constructors, and other nested
        // classifiers after the enum entries.
        self.nested_classes(header, true, &mut declarations)?;
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
        self.nested_classes(header, false, &mut declarations)?;
        if header.kind == KspClassKind::EnumClass {
            declarations.extend(self.enum_members(header));
        }
        Ok((declarations, complete))
    }

    /// Adds the classifiers nested in `header`'s class that are `inner` or not, in source order.
    fn nested_classes(
        &self,
        header: &ClassHeader<'_>,
        inner: bool,
        declarations: &mut Vec<KspDeclaration>,
    ) -> Result<(), String> {
        for &nested in &self.file.decls {
            if !self.is_member_of(nested, Some(header.id)) {
                continue;
            }
            if let Decl::Class(nested_class) = self.file.decl(nested) {
                if nested_class.inner_of.is_some() == inner {
                    declarations.push(KspDeclaration::Class(self.class(
                        nested,
                        nested_class,
                        header.scope,
                    )?));
                }
            }
        }
        Ok(())
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
        let spellings = signature
            .and_then(|signature| signature.stable_declaration)
            .and_then(|declaration| self.analysis.declared_type_spellings(declaration));
        let return_type = signature
            .and_then(|signature| scope.ty(generic.map_or(signature.ret, |generic| generic.ret)))
            .and_then(|ty| match &function.ret {
                Some(spelling) => as_written(
                    self.file,
                    spelling,
                    spellings.map(|spellings| &spellings.ret),
                    ty,
                ),
                None => Some(ty),
            });
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
        let jvm_parameters: Vec<Option<Ty>> = (function.context_count..function.params.len())
            .map(|index| {
                let resolved = parameter_types?[index];
                declared_parameter_type(resolved, function.params[index].is_vararg)
            })
            .collect();
        let jvm_result =
            signature.map(|signature| generic.map_or(signature.ret, |generic| generic.ret));
        let (jvm_signature, jvm_name) = self.jvm_function(
            &FunctionNaming {
                name: &function.name,
                annotations: self.applied(declaration),
                site: NamingSite {
                    internal: function.visibility == Visibility::Internal,
                    member: is_member,
                    bare_accessors: false,
                    generated: false,
                },
            },
            &jvm_parameters,
            jvm_result,
        );
        let mut parameters = Vec::with_capacity(function.params.len());
        for (index, parameter) in function
            .params
            .iter()
            .enumerate()
            .skip(function.context_count)
        {
            let resolved = parameter_types.map(|types| types[index]);
            let mut described = parameter_description(
                self.file,
                parameter,
                resolved,
                spellings.map(|spellings| spellings.param(index)),
                &scope,
            );
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
            // A member is abstract when it says so or is an interface's without a body; an
            // `external` one has no body either and is not.
            is_abstract: match owner {
                Owner::Class { header, .. } => {
                    function.is_abstract()
                        || (header.kind == KspClassKind::Interface
                            && matches!(function.body, FunBody::None))
                }
                Owner::TopLevel { .. } => false,
            },
            origin: KspDeclarationOrigin::Source,
            line: Some(function.decl_line),
            annotations: self.published_annotations(
                subject,
                &function.annotations,
                declaration.map(|declaration| self.analysis.declaration_annotations(declaration)),
                Placement::Declaration,
            )?,
            modifiers: self.written_modifiers(function.declaration_span.lo),
            type_parameters,
            extension_receiver: receiver_slot(
                self.file,
                function.receiver.as_ref(),
                receiver,
                spellings.map(|spellings| &spellings.receiver),
                &scope,
            ),
            parameters,
            return_type,
            jvm_signature,
            jvm_name,
        })
    }

    /// The annotations the published declaration `declaration` has.
    fn applied(&self, declaration: Option<FrontendDeclarationId>) -> &[ResolvedAnnotation] {
        declaration.map_or(&[], |declaration| {
            self.analysis.declaration_annotations(declaration)
        })
    }

    /// The JVM signature and name of a function with `parameters` and `result`.
    fn jvm_function(
        &self,
        naming: &FunctionNaming<'_>,
        parameters: &[Option<Ty>],
        result: Option<Ty>,
    ) -> (Option<String>, Option<String>) {
        let Some(platform) = self.platform else {
            return (None, None);
        };
        (
            platform.function_signature(parameters, result),
            platform.function_name(naming, parameters, result),
        )
    }

    /// Adds the JVM signature and name of a constructor of `header`'s class with `parameters`:
    /// KSP2 spells it returning `Unit`, and mangles its name as returning the class.
    fn add_constructor_jvm_names(
        &self,
        constructor: &mut KspFunction,
        header: &ClassHeader<'_>,
        parameters: &[Option<Ty>],
        declaration: Option<FrontendDeclarationId>,
    ) {
        let Some(platform) = self.platform else {
            return;
        };
        constructor.jvm_signature = platform.function_signature(parameters, Some(Ty::Unit));
        constructor.jvm_name = platform.function_name(
            &FunctionNaming {
                name: "<init>",
                annotations: self.applied(declaration),
                site: NamingSite {
                    internal: false,
                    member: true,
                    bare_accessors: false,
                    generated: false,
                },
            },
            parameters,
            Some(header.class_ty()),
        );
    }

    /// Adds the JVM signature and name of a function the language contributes.
    fn add_synthetic_jvm_names(
        &self,
        function: &mut KspFunction,
        parameters: &[Option<Ty>],
        result: Option<Ty>,
    ) {
        let name = function.name.clone();
        (function.jvm_signature, function.jvm_name) = self.jvm_function(
            &FunctionNaming {
                name: &name,
                annotations: &[],
                site: NamingSite {
                    internal: false,
                    member: true,
                    bare_accessors: false,
                    generated: true,
                },
            },
            parameters,
            result,
        );
    }

    /// Adds the JVM signature of `property`, of type `ty`, and the JVM names of its accessors.
    fn add_jvm_names(
        &self,
        property: &mut KspProperty,
        ty: Option<Ty>,
        declaration: Option<FrontendDeclarationId>,
        site: NamingSite,
    ) {
        let Some(platform) = self.platform else {
            return;
        };
        let accessor_annotations = |setter: bool| {
            declaration.map_or(&[][..], |declaration| {
                self.analysis.accessor_annotations(declaration, setter)
            })
        };
        property.jvm_signature = ty.and_then(|ty| platform.type_signature(ty));
        if let Some(getter) = &mut property.getter {
            getter.jvm_name = platform.accessor_name(
                &property.name,
                false,
                ty,
                accessor_annotations(false),
                site,
            );
        }
        if let Some(setter) = &mut property.setter {
            setter.jvm_name =
                platform.accessor_name(&property.name, true, ty, accessor_annotations(true), site);
        }
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
        let (receiver, resolved) = match owner {
            Owner::TopLevel { id, .. } if property.receiver.is_some() => {
                match self.symbols.source_extension_property((self.index, id.0)) {
                    Some(resolved) => (Some(Some(resolved.receiver)), Some(resolved.ty)),
                    None => (Some(None), None),
                }
            }
            Owner::TopLevel { id, .. } => (
                None,
                self.symbols
                    .source_props
                    .get(&(self.index, id.0))
                    .map(|resolved| resolved.ty),
            ),
            // A member extension property is not described yet.
            Owner::Class { .. } if property.receiver.is_some() => (Some(None), None),
            Owner::Class { header, .. } => (
                None,
                header
                    .signature
                    .declared_props
                    .get(&property.name)
                    .map(|resolved| resolved.ty),
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
        let spellings =
            declaration.and_then(|declaration| self.analysis.declared_type_spellings(declaration));
        let ty = resolved
            .and_then(|ty| scope.ty(ty))
            .and_then(|ty| match &property.ty {
                Some(spelling) => as_written(
                    self.file,
                    spelling,
                    spellings.map(|spellings| &spellings.ret),
                    ty,
                ),
                None => Some(ty),
            });
        let modifiers = self.written_modifiers(property.declaration_span.lo);
        let (getter, setter) =
            self.written_accessors(subject, property, declaration, modifiers.as_deref())?;
        let mut described = KspProperty {
            name: property.name.clone(),
            is_mutable: property.is_var,
            origin: KspDeclarationOrigin::Source,
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
            extension_receiver: receiver_slot(
                self.file,
                property.receiver.as_ref(),
                receiver,
                spellings.map(|spellings| &spellings.receiver),
                scope,
            ),
            ty,
            getter: Some(getter),
            setter,
            has_backing_field: super::backing_field::source_property_has_backing_field(
                property,
                matches!(owner, Owner::Class { header, .. } if header.kind == KspClassKind::Interface),
            ),
            jvm_signature: None,
        };
        self.add_jvm_names(
            &mut described,
            resolved,
            declaration,
            NamingSite {
                internal: property.visibility == Visibility::Internal,
                member: matches!(owner, Owner::Class { .. }),
                bare_accessors: match owner {
                    Owner::Class { header, .. } => header.bare_accessors(),
                    _ => false,
                },
                generated: false,
            },
        );
        Ok(described)
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
        // KSP2 lists a `vararg` parameter's modifier on the property it declares too.
        let modifiers = self.written_modifiers(parameter.declaration_span.lo);
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
        let mut described = KspProperty {
            name: parameter.name.clone(),
            is_mutable: parameter.is_var,
            origin: KspDeclarationOrigin::Source,
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
            ty: header.constructor_property_type(index),
            getter: Some(getter),
            setter,
            has_backing_field: Some(true),
            jvm_signature: None,
        };
        self.add_jvm_names(
            &mut described,
            header.constructor_property(index),
            declaration,
            NamingSite {
                internal: parameter.visibility == Visibility::Internal,
                member: true,
                bare_accessors: header.bare_accessors(),
                generated: false,
            },
        );
        Ok(described)
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
        for (index, ((name, has_default), written)) in signature
            .ctor_param_names
            .iter()
            .zip(&class.props)
            .enumerate()
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
            Placement::Declaration,
        )?;
        let mut primary = constructor(
            header,
            KspFunctionKind::PrimaryConstructor,
            written.then_some(class.primary_ctor_lines.decl_line),
            annotations,
            Some(constructor_modifiers(header.kind, written, visibility)),
            parameters,
        );
        let types: Vec<Option<Ty>> = (0..class.props.len())
            .map(|index| header.constructor_parameter(index))
            .collect();
        self.add_constructor_jvm_names(&mut primary, header, &types, declaration);
        Ok(Some(primary))
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
        let declaration = header.signature.stable_declaration.and_then(|class| {
            self.analysis
                .constructor_declaration(class, index as u32 + 1)
        });
        let spellings =
            declaration.and_then(|declaration| self.analysis.declared_type_spellings(declaration));
        let mut parameters = Vec::with_capacity(constructor_decl.params.len());
        for (position, parameter) in constructor_decl.params.iter().enumerate() {
            let resolved = resolved.map(|types| types[position]);
            let mut described = parameter_description(
                self.file,
                parameter,
                resolved,
                spellings.map(|spellings| spellings.param(position)),
                header.scope,
            );
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
        let mut secondary = constructor(
            header,
            KspFunctionKind::SecondaryConstructor,
            Some(constructor_decl.decl_line),
            self.published_annotations(
                subject,
                &constructor_decl.annotations,
                declaration.map(|declaration| self.analysis.declaration_annotations(declaration)),
                Placement::Declaration,
            )?,
            modifiers,
            parameters,
        );
        let types: Vec<Option<Ty>> = constructor_decl
            .params
            .iter()
            .enumerate()
            .map(|(position, parameter)| {
                declared_parameter_type(resolved?[position], parameter.is_vararg)
            })
            .collect();
        self.add_constructor_jvm_names(&mut secondary, header, &types, declaration);
        Ok(secondary)
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
            // KSP2 spells an entry as its enum class.
            jvm_signature: self
                .platform
                .and_then(|platform| platform.type_signature(header.class_ty())),
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
                let mut function = synthetic_function(
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
                );
                self.add_synthetic_jvm_names(
                    &mut function,
                    &[],
                    header.constructor_parameter(*index),
                );
                KspDeclaration::Function(function)
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
                    unannotated: false,
                })
            })
            .collect();
        let copy_types: Vec<Option<Ty>> = properties
            .iter()
            .map(|(index, _)| header.constructor_parameter(*index))
            .collect();
        let mut copy = synthetic_function(
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
        );
        self.add_synthetic_jvm_names(&mut copy, &copy_types, Some(header.class_ty()));
        members.push(KspDeclaration::Function(copy));
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
        let mut values = synthetic_function(
            "values".to_string(),
            KspFunctionKind::Static,
            modifiers.clone(),
            Vec::new(),
            Some(applied("kotlin.Array")),
        );
        self.add_synthetic_jvm_names(&mut values, &[], Some(Ty::array(header.class_ty())));
        let mut value_of = synthetic_function(
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
        );
        self.add_synthetic_jvm_names(&mut value_of, &[Some(Ty::String)], Some(header.class_ty()));
        let mut members = vec![
            KspDeclaration::Function(values),
            KspDeclaration::Function(value_of),
        ];
        if self.file.enum_entries_enabled {
            let mut entries = KspProperty {
                name: "entries".to_string(),
                is_mutable: false,
                origin: KspDeclarationOrigin::Synthetic,
                line: None,
                annotations: Vec::new(),
                modifiers: Some(modifiers),
                type_parameters: Some(Vec::new()),
                extension_receiver: KspTypeSlot::Absent,
                ty: Some(applied("kotlin.enums.EnumEntries")),
                getter: Some(super::accessors::compiler_getter()),
                setter: None,
                has_backing_field: Some(false),
                jvm_signature: None,
            };
            let entries_ty = Ty::obj_args_name(
                crate::types::type_name("kotlin/enums/EnumEntries"),
                &[header.class_ty()],
            );
            let site = NamingSite {
                internal: false,
                member: true,
                bare_accessors: false,
                generated: true,
            };
            self.add_jvm_names(&mut entries, Some(entries_ty), None, site);
            members.push(KspDeclaration::Property(Box::new(entries)));
        }
        members
    }
}

/// A written parameter described with its `resolved` type.
fn parameter_description(
    file: &File,
    parameter: &Param,
    resolved: Option<Ty>,
    spelled: Option<&Spelled>,
    scope: &TypeParameterScope,
) -> KspParameter {
    KspParameter {
        name: parameter.name.clone(),
        ty: resolved
            .and_then(|ty| declared_parameter_type(ty, parameter.is_vararg))
            .and_then(|ty| scope.ty(ty))
            .and_then(|ty| as_written(file, &parameter.ty, spelled, ty)),
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

/// The extension receiver slot of a declaration that writes `receiver`, resolved or not, as
/// `spelling` writes it.
fn receiver_slot(
    file: &File,
    spelling: Option<&TypeRef>,
    receiver: Option<Option<Ty>>,
    spelled: Option<&Spelled>,
    scope: &TypeParameterScope,
) -> KspTypeSlot {
    match receiver {
        None => KspTypeSlot::Absent,
        Some(resolved) => resolved
            .and_then(|ty| scope.ty(ty))
            .and_then(|ty| match spelling {
                Some(spelling) => as_written(file, spelling, spelled, ty),
                None => Some(ty),
            })
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
        // A constructor the source does not write is the compiler's.
        origin: match line {
            Some(_) => KspDeclarationOrigin::Source,
            None => KspDeclarationOrigin::Synthetic,
        },
        line,
        annotations,
        modifiers,
        type_parameters: header.type_parameters.clone(),
        extension_receiver: KspTypeSlot::Absent,
        parameters,
        return_type: Some(header.star_projected()),
        jvm_signature: None,
        jvm_name: None,
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
        origin: KspDeclarationOrigin::Synthetic,
        line: None,
        annotations: Vec::new(),
        modifiers: Some(modifiers),
        type_parameters: Some(Vec::new()),
        extension_receiver: KspTypeSlot::Absent,
        parameters,
        return_type,
        jvm_signature: None,
        jvm_name: None,
    }
}
