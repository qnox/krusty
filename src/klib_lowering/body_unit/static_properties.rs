//! Top-level properties of a dependency library in this closed unit.
//!
//! An accessor of a top-level property is an ordinary top-level function of the unit, lowered
//! from its serialized body. A property with a backing field keeps that field as one static of
//! the unit, which the property's own library reads and writes: the accessor bodies, and nothing
//! outside that library, name the field. Its initializer must be a constant, which a target may
//! store before the unit's first function runs without any program observing the difference.

use super::*;
use crate::ir::{IrConst, IrExpr, IrStatic, IrStaticAccessor, IrStaticAccessors};
use crate::klib_libraries::LocatedStaticField;
use crate::metadata::klib_ir::tree::{KlibIrExprKind, KlibIrProperty};

/// The library a backing field belongs to and its serialized symbol.
pub(super) type StaticIdentity = (String, KlibIrSymbol);

impl Linker<'_, '_> {
    /// The accessor `identity` of a top-level property this unit lowers from its body, linked as
    /// the unit function it is declared as. `None` when `identity` is no top-level accessor.
    pub(super) fn top_level_accessor(
        &mut self,
        ir: &mut IrFile,
        identity: &KlibDependencyCallable,
    ) -> Result<Option<LinkedAccessor>, KlibBodyDeclineReason> {
        let Some((_, property, write)) = self
            .sources
            .bodies
            .top_level_accessor_property(identity)
            .map_err(|reason| KlibBodyDeclineReason::SignatureMismatch(reason.to_owned()))?
        else {
            return Ok(None);
        };
        let (arena, declaration) = self
            .sources
            .bodies
            .function(identity)
            .ok_or(KlibBodyDeclineReason::Unjoined)?;
        if excluded_from_codegen(&property.base.annotations)
            || excluded_from_codegen(&declaration.base.annotations)
        {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a declaration excluded from code generation",
            ));
        }
        let selected = match self.sources.callees.get(identity) {
            None => None,
            Some(KlibCalleeFact::Ambiguous) => {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "conflicting provider contracts for a property accessor".to_owned(),
                ))
            }
            Some(KlibCalleeFact::Selected(callables)) => {
                Some(SelectedKlibCallables::Variants(callables))
            }
        };
        let selected = selected_callable_for_header(selected, None, arena, declaration)?;
        let function = match self.functions.get(identity) {
            Some(&function) => {
                if let Some(selected) = selected {
                    function_header(Some(selected), arena, declaration)?;
                }
                function
            }
            None => self.declare(ir, identity, selected, arena, declaration)?,
        };
        let declared = &ir.functions[function as usize];
        Ok(Some(LinkedAccessor {
            target: LinkedAccessorTarget::Local(function),
            write,
            receiver: None,
            params: declared.params.clone(),
            ret: declared.ret,
            context_count: ir.declared_parameters(function).context_count,
            class_type_parameters: Vec::new(),
        }))
    }

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
        // The unit initializes its statics from their constants alone, before any export runs,
        // so the defining file's initialization must have no other effect to preserve.
        let KlibIrSignature::FileLocal { file, .. } = backing.base.symbol.signature else {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a top-level backing field without a file-local identity".to_owned(),
            ));
        };
        if !self
            .sources
            .bodies
            .module_file_initialization_is_inert(&self.active_module, file)
        {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a top-level property of a file whose initialization runs code",
            ));
        }
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
        let init = constant_initializer(ir, arena, backing, ty)?;
        let index = u32::try_from(ir.statics.len()).expect("a static list fits u32");
        ir.statics.push(IrStatic {
            name: backing.name.clone(),
            ty,
            init: Some(init),
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
        });
        self.statics.insert(identity, index);
        Ok(Some(StaticField {
            index,
            ty,
            writable: !flags.is_final,
        }))
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
/// nullable field, or a value of exactly that type.
fn constant_initializer(
    ir: &mut IrFile,
    arena: &KlibIrArena,
    field: &KlibIrField,
    ty: Ty,
) -> Result<crate::ir::ExprId, KlibBodyDeclineReason> {
    let initializer = field
        .initializer
        .ok_or(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a top-level property without an initializer",
        ))?;
    let expression = arena.expr(initializer);
    let KlibIrExprKind::Const(constant) = &expression.kind else {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "a top-level property whose initializer runs code",
        ));
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
    Ok(init)
}
