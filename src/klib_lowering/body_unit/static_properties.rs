//! Top-level properties of a dependency library in this closed unit.
//!
//! An accessor of a top-level property is an ordinary top-level function of the unit, lowered
//! from its serialized body. A property with a backing field keeps that field as one static of
//! the unit, which the property's own library reads and writes: the accessor bodies, and nothing
//! outside that library, name the field.
//!
//! Any other initializer runs as part of its file's initialization, as kotlinc initializes a
//! file: once, when a top-level function or accessor of that file is first called, every such
//! initializer of the file in declaration order. The flag is set before the initializers run, so
//! one that calls back into its own file sees the defaults of the properties not yet initialized.
//!
//! A constant initializer is the static's initial value where its file's initialization runs no
//! code, since no program can then observe when it is stored, and on a target that stores every
//! constant before any code of its file runs, as Kotlin/Native does. On any other target, as
//! kotlinc-wasm does, a constant is stored in its place in its file's initialization order, so an
//! earlier initializer reading it sees the default.

use super::*;
use crate::ir::{IrConst, IrExpr, IrStatic, IrStaticAccessor, IrStaticAccessors};
use crate::klib_libraries::LocatedStaticField;
use crate::metadata::klib_ir::tree::{KlibIrExprKind, KlibIrProperty};

/// The library a backing field belongs to and its serialized symbol.
pub(super) type StaticIdentity = (String, KlibIrSymbol);

/// A library and one of its serialized files.
pub(super) type FileIdentity = (String, u32);

impl<'s> Linker<'_, 's> {
    /// The accessor `identity` of a top-level property this unit lowers from its body, linked as
    /// the top-level function of the unit it is declared as, generic or not. `None` when an
    /// earlier unit already emitted it, or when `identity` is no top-level accessor with a body.
    ///
    /// The serialized declaration must hold its role's slot of the property group its signature
    /// names. A provider selection, when a checked call made one, names that accessor in every
    /// variant; the accessor of a file-private property has none.
    pub(super) fn top_level_accessor(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<LinkedCallee>, KlibBodyDeclineReason> {
        if self.sources.externals.contains_key(identity) {
            return Ok(None);
        }
        let mismatch = |reason: &str| KlibBodyDeclineReason::SignatureMismatch(reason.to_owned());
        let Some((_, property, setter)) = self
            .sources
            .bodies
            .top_level_accessor_property(identity)
            .map_err(mismatch)?
        else {
            return Ok(None);
        };
        let role = if setter {
            crate::libraries::KlibAccessorRole::Setter
        } else {
            crate::libraries::KlibAccessorRole::Getter
        };
        let selected = match self.sources.callees.get(identity) {
            None => None,
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(mismatch(
                    "conflicting provider contracts for a property accessor",
                ))
            }
            Some(KlibCalleeFact::Selected(callables)) => {
                let named = callables[0].accessor().map(|accessor| &accessor.property);
                if callables.iter().any(|callable| {
                    callable.accessor().is_none_or(|accessor| {
                        Some(&accessor.property) != named || accessor.role != role
                    })
                }) {
                    return Err(mismatch(
                        "the selected variants name other accessors of the property",
                    ));
                }
                Some(SelectedKlibCallables::Variants(callables))
            }
        };
        let (arena, declaration) = self
            .sources
            .bodies
            .top_level_accessor_header(identity, role)
            .map_err(mismatch)?
            .ok_or(KlibBodyDeclineReason::Unjoined)?;
        if excluded_from_codegen(&property.base.annotations)
            || excluded_from_codegen(&declaration.base.annotations)
        {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a declaration excluded from code generation",
            ));
        }
        if !matches!(declaration.body, Some(KlibIrBody::Block(_))) {
            return Ok(None);
        }
        self.top_level_function(ir, identity, selected, arena, declaration)
            .map(Some)
    }
}

impl Linker<'_, '_> {
    /// The static of the top-level property whose backing field is `field`, declared the first
    /// time a body of the property's library reaches it. `None` when the active library declares
    /// no such property.
    pub(super) fn static_storage(
        &mut self,
        ir: &mut IrFile,
        field: &KlibIrSymbol,
    ) -> Result<Option<StaticField>, KlibBodyDeclineReason> {
        let identity = (self.active_module.clone(), field.clone());
        if let Some(&index) = self.statics.get(&identity) {
            let declared = &ir.statics[index as usize];
            return Ok(Some(StaticField {
                index,
                ty: declared.ty,
                writable: declared.is_var,
            }));
        }
        let (arena, property, backing) = match self
            .sources
            .bodies
            .module_static_field(&self.active_module, field)
        {
            LocatedStaticField::Missing => return Ok(None),
            LocatedStaticField::Several => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a static field has an invalid or repeated storage identity".to_owned(),
                ))
            }
            LocatedStaticField::One {
                arena,
                property,
                field,
            } => (arena, property, field),
        };
        let flags =
            backing
                .static_storage_flags()
                .ok_or(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "an external or unknown static field storage flag",
                ))?;
        let KlibIrSignature::FileLocal { file, .. } = backing.base.symbol.signature else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a top-level backing field without a file-local identity".to_owned(),
            ));
        };
        if property.is_lateinit() {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a `lateinit` top-level property",
            ));
        }
        let ty = super::super::klib_types::semantic_type(
            arena,
            &super::super::klib_types::KlibTypeParameters::default(),
            backing.ty,
        )?;
        // Any other initializer is its file initialization's to store, as is a constant whose
        // file initialization runs code on a target that does not store it first.
        let init = constant_initializer(ir, arena, backing, ty)?;
        let init = init.filter(|_| {
            self.presets_constant_statics
                || self
                    .sources
                    .bodies
                    .module_file_initialization_is_inert(&self.active_module, file)
        });
        let index = u32::try_from(ir.statics.len()).expect("a static list fits u32");
        ir.statics.push(IrStatic {
            name: backing.name.clone(),
            ty,
            init,
            is_var: !flags.is_final,
            is_const: false,
            is_lateinit: false,
            owner: None,
            visibility: crate::metadata::semantic::declaration_visibility(property.base.flags),
            erased_declared_ty: None,
            accessors: accessors(property),
            line: 0,
            accessor_lines: crate::ir::IrAccessorLines::default(),
            source_order: u32::MAX,
            property_delegate: false,
            deferred: false,
        });
        self.statics.insert(identity, index);
        Ok(Some(StaticField {
            index,
            ty,
            writable: !flags.is_final,
        }))
    }
}

impl Linker<'_, '_> {
    /// Make every top-level function this unit lowered since the last call run its file's
    /// initialization first, when that file's initialization runs code. Declaring an
    /// initialization queues the bodies of its initializers.
    pub(super) fn initialize_files(
        &mut self,
        ir: &mut IrFile,
    ) -> Result<(), KlibBodyDeclineReason> {
        let lowered = self
            .functions
            .iter()
            .filter(|(_, function)| !self.initialized_functions.contains(function))
            .map(|(identity, &function)| (identity.clone(), function))
            .collect::<Vec<_>>();
        for (identity, function) in lowered {
            self.initialized_functions.insert(function);
            let Some(file) = self.sources.bodies.top_level_file(&identity) else {
                continue;
            };
            if self
                .sources
                .bodies
                .module_file_initialization_is_inert(&identity.module, file)
            {
                continue;
            }
            let initialization = self.file_initialization(ir, (identity.module, file))?;
            let Some(body) = ir.functions[function as usize].body else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a top-level function of the unit has no lowered body".to_owned(),
                ));
            };
            let call = initialization_call(ir, initialization);
            let IrExpr::Block { stmts, .. } = &mut ir.exprs[body as usize] else {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a lowered body is not a block".to_owned(),
                ));
            };
            stmts.insert(0, call);
        }
        Ok(())
    }

    /// The function that initializes `file`, declared with the static flag it sets the first time
    /// a body reaches the file, and the initializer of each of its properties queued.
    fn file_initialization(
        &mut self,
        ir: &mut IrFile,
        file: FileIdentity,
    ) -> Result<FunId, KlibBodyDeclineReason> {
        if let Some(&function) = self.file_initializations.get(&file) {
            return Ok(function);
        }
        let flag = u32::try_from(ir.statics.len()).expect("a static list fits u32");
        let unset = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        ir.set_logical_type(unset, Ty::Boolean);
        ir.statics.push(IrStatic {
            name: "fileInitialized".to_owned(),
            ty: Ty::Boolean,
            init: Some(unset),
            is_var: true,
            is_const: false,
            is_lateinit: false,
            owner: None,
            visibility: crate::types::Visibility::Private,
            erased_declared_ty: None,
            accessors: IrStaticAccessors {
                getter: IrStaticAccessor::Absent,
                setter: IrStaticAccessor::Absent,
            },
            line: 0,
            accessor_lines: crate::ir::IrAccessorLines::default(),
            source_order: u32::MAX,
            property_delegate: false,
            deferred: false,
        });
        let header = super::super::function_lowering::initialization_header("initializeFile");
        let initialization = super::super::function_lowering::declare_function(ir, &header);
        self.file_initializations
            .insert(file.clone(), initialization);
        let mut steps = Vec::new();
        for initializer in self.sources.bodies.module_file_initializers(
            &file.0,
            file.1,
            !self.presets_constant_statics,
        ) {
            let saved = std::mem::replace(&mut self.active_module, file.0.clone());
            let storage = self.static_storage(ir, &initializer.field.base.symbol);
            self.active_module = saved;
            let field = storage?.ok_or_else(|| {
                KlibBodyDeclineReason::SignatureMismatch(
                    "a top-level property's backing field is not its library's static".to_owned(),
                )
            })?;
            let header = super::super::function_lowering::initialization_header(&format!(
                "initialize{}",
                initializer.property.name
            ));
            let function = super::super::function_lowering::declare_function(ir, &header);
            self.pending.push_back(PendingBody {
                identity: KlibDependencyCallable {
                    module: file.0.clone(),
                    declaration: field_declaration(&initializer.field.base.symbol)?,
                },
                arena: initializer.arena,
                symbol: &initializer.property.base.symbol,
                statements: &[],
                header,
                function,
                initialization: None,
                value_member: None,
                local_body: None,
                static_initialization: Some((field, initializer.initializer)),
            });
            steps.push(initialization_call(ir, function));
        }
        // Once set, the flag makes every later call return at once, a call from the file's own
        // initializers included.
        let read = ir.add_expr(IrExpr::GetStatic(flag));
        ir.set_logical_type(read, Ty::Boolean);
        let done = ir.add_expr(IrExpr::Return(None));
        ir.set_logical_type(done, Ty::Nothing);
        let check = ir.add_expr(IrExpr::When {
            branches: vec![(Some(read), done)],
        });
        ir.set_logical_type(check, Ty::Unit);
        let set = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        ir.set_logical_type(set, Ty::Boolean);
        let mark = ir.add_expr(IrExpr::SetStatic {
            index: flag,
            value: set,
        });
        ir.set_logical_type(mark, Ty::Unit);
        let body = ir.add_expr(IrExpr::Block {
            stmts: [check, mark].into_iter().chain(steps).collect(),
            value: None,
        });
        ir.callable_scopes.insert(body);
        ir.functions[initialization as usize].body = Some(body);
        Ok(initialization)
    }
}

/// A call of the initialization function `function`, a statement.
fn initialization_call(ir: &mut IrFile, function: FunId) -> crate::ir::ExprId {
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(function)),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    ir.set_logical_type(call, Ty::Unit);
    call
}

/// The declaration a backing field's initialization reports a decline as: the field itself.
fn field_declaration(
    field: &KlibIrSymbol,
) -> Result<KlibDeclarationSignature, KlibBodyDeclineReason> {
    match field.signature {
        KlibIrSignature::FileLocal { file, slot } => {
            Ok(KlibDeclarationSignature::FileLocal { file, slot })
        }
        KlibIrSignature::Public(_)
        | KlibIrSignature::Accessor(_)
        | KlibIrSignature::TypeParameter { .. } => Err(KlibBodyDeclineReason::SignatureMismatch(
            "a top-level backing field without a file-local identity".to_owned(),
        )),
    }
}

/// Whether `annotations` exclude their declaration from code generation: it has no code of its
/// own, and the compiler realizes each use of it, as the Wasm stdlib's `implementedAsIntrinsic`
/// placeholder is.
fn excluded_from_codegen(annotations: &[crate::metadata::klib_ir::tree::KlibIrAnnotation]) -> bool {
    let excluded = crate::types::type_name("kotlin/wasm/internal/ExcludedFromCodegen");
    annotations.iter().any(|annotation| {
        super::super::klib_types::annotation_classifier(annotation) == Some(excluded)
    })
}

/// Every accessor the property serializes is an ordinary function of the unit, lowered from its
/// body with the field read as the static.
fn accessors(property: &KlibIrProperty) -> IrStaticAccessors {
    let accessor = |declared: Option<_>| {
        if declared.is_some() {
            IrStaticAccessor::Declared
        } else {
            IrStaticAccessor::Absent
        }
    };
    IrStaticAccessors {
        getter: accessor(property.getter),
        setter: accessor(property.setter),
    }
}

/// The constant a backing field is initialized with, of the field's own type `ty`: `null` for a
/// nullable field, or a value of exactly that type. `None` when the initializer runs code, which
/// the file's initialization stores instead.
fn constant_initializer(
    ir: &mut IrFile,
    arena: &KlibIrArena,
    field: &KlibIrField,
    ty: Ty,
) -> Result<Option<crate::ir::ExprId>, KlibBodyDeclineReason> {
    let initializer = field
        .initializer
        .ok_or(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a top-level property without an initializer",
        ))?;
    let expression = arena.expr(initializer);
    let KlibIrExprKind::Const(constant) = &expression.kind else {
        return Ok(None);
    };
    let serialized = expression
        .ty
        .ok_or(KlibBodyDeclineReason::UntypedExpression("constant"))?;
    let serialized = super::super::klib_types::semantic_type(
        arena,
        &super::super::klib_types::KlibTypeParameters::default(),
        serialized,
    )?;
    let (value, value_ty) = constant_value(constant, serialized)?;
    let stored = match value {
        IrConst::Null => ty.is_nullable(),
        _ => value_ty == ty,
    };
    if !stored {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "a top-level property's constant is not of its field's type".to_owned(),
        ));
    }
    let init = ir.add_expr(IrExpr::Const(value));
    ir.set_logical_type(init, value_ty);
    Ok(Some(init))
}
