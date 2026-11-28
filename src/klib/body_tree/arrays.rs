//! Arrays: the builtin array members common IR's array intrinsics realize, sized primitive
//! arrays, array literals, and `vararg` arguments.

use super::{BodyExprId, BodyKind, CallTarget, Translation};
use crate::ir::{ExprId, IrIntrinsic};
use crate::metadata::id_signature::{
    accessor_signature, callable_signature, Accessor, CallableShape, ClassScope,
    DeclarationContainer, Placement, PropertyShape, TypeParameterShape,
};
use crate::metadata::semantic::KotlinType;
use crate::types::{Ty, TyKind};

impl Translation<'_, '_> {
    /// `IntArray(size)` and its siblings: the primitive array class's sized constructor.
    pub(super) fn new_array(&mut self, array_type: Ty, size: ExprId) -> Option<BodyExprId> {
        if array_type.is_reference_array() || !array_type.is_array() {
            return None;
        }
        let size = self.expr(size)?;
        if self.ty(size) != Ty::Int {
            return None;
        }
        let target =
            builtin_class_member(array_type, "<init>", &[KotlinType::class("kotlin/Int")])?;
        self.push(
            array_type,
            BodyKind::ConstructorCall {
                target,
                constructor_type_arguments: 0,
                type_arguments: Vec::new(),
                arguments: vec![size],
                origin: None,
            },
        )
    }

    /// The elements of a `vararg` argument, of the parameter's array type.
    pub(super) fn vararg(
        &mut self,
        array_type: Ty,
        spreads: &[bool],
        elements: &[ExprId],
    ) -> Option<BodyExprId> {
        let element = array_type.array_elem()?;
        let element = match element.kind() {
            TyKind::OutProjection(inner) => inner,
            TyKind::InProjection(_) | TyKind::StarProjection(_) => return None,
            _ => element,
        };
        // A reference vararg is an `Array<out E>`, as its parameter is declared.
        let array_type = if array_type.is_reference_array() {
            Ty::obj_args_name(crate::types::wk::array(), &[Ty::out_projection(element)])
        } else {
            array_type
        };
        if spreads.len() != elements.len() || !self.supported(array_type) {
            return None;
        }
        let elements = elements
            .iter()
            .zip(spreads)
            .map(|(element, spread)| Some((self.expr(*element)?, *spread)))
            .collect::<Option<Vec<_>>>()?;
        self.push(array_type, BodyKind::Vararg { element, elements })
    }

    /// An array literal, which Kotlin IR writes as the call of its factory (`intArrayOf`,
    /// `arrayOf<T>`) passing the elements as the factory's `vararg`.
    pub(super) fn array_factory(
        &mut self,
        array_type: Ty,
        spreads: &[bool],
        elements: &[ExprId],
    ) -> Option<BodyExprId> {
        let element = array_type.array_elem()?;
        let (target, type_arguments, parameter) = if array_type.is_reference_array() {
            if matches!(
                element.kind(),
                TyKind::OutProjection(_) | TyKind::InProjection(_) | TyKind::StarProjection(_)
            ) {
                return None;
            }
            (reference_array_factory()?, vec![element], array_type)
        } else {
            (primitive_array_factory(array_type)?, Vec::new(), array_type)
        };
        let vararg = self.vararg(parameter, spreads, elements)?;
        self.push(
            array_type,
            BodyKind::Call {
                target,
                type_arguments,
                arguments: vec![vararg],
                origin: None,
            },
        )
    }
}

/// The member of a builtin array class an array intrinsic of common IR realizes: `get`, `set`, or
/// the getter of `size`. `Array<T>`'s `set` takes its class's own type parameter.
pub(super) fn array_member(array: Ty, operation: IrIntrinsic) -> Option<CallTarget> {
    let array = array.non_null();
    if !array.is_array() {
        return None;
    }
    let element = if array.is_reference_array() {
        KotlinType::Param {
            name: "T".to_owned(),
            id: crate::metadata::semantic::KotlinTypeParameterId(0),
            nullable: false,
        }
    } else {
        // Boundary conversion: the mangler signs the metadata class id.
        KotlinType::class(array.array_elem()?.obj_internal()?.render())
    };
    let index = KotlinType::class("kotlin/Int");
    match operation {
        IrIntrinsic::ArrayGet => builtin_class_member(array, "get", &[index]),
        IrIntrinsic::ArraySet => builtin_class_member(array, "set", &[index, element]),
        IrIntrinsic::ArraySize => {
            let class = array.obj_internal()?;
            let (package, classes) = builtin_class_scope(array, &class)?;
            let property = PropertyShape::<KotlinType> {
                name: "size",
                contexts: Vec::new(),
                receiver: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            };
            let signature = accessor_signature(
                DeclarationContainer {
                    package: &package,
                    classes: &classes,
                    native_interop_library: false,
                },
                &property,
                Accessor::Getter,
            )
            .ok()?;
            Some(CallTarget::Accessor(signature))
        }
        _ => None,
    }
}

/// The package and class scope of a builtin `kotlin` class, with `Array`'s one type parameter.
pub(super) fn builtin_class_scope(
    ty: Ty,
    class: &crate::types::TypeName,
) -> Option<([String; 1], [ClassScope<'_, KotlinType>; 1])> {
    if !class.package_matches("kotlin") {
        return None;
    }
    let type_parameters = if ty.is_reference_array() {
        vec![TypeParameterShape {
            id: crate::metadata::semantic::KotlinTypeParameterId(0),
            bounds: Vec::new(),
        }]
    } else {
        Vec::new()
    };
    Some((
        ["kotlin".to_owned()],
        [ClassScope {
            name: class.segment_ref(),
            type_parameters,
            expect: false,
        }],
    ))
}

/// The function or constructor `name` of a builtin `kotlin` class, over `params`.
pub(super) fn builtin_class_member(
    ty: Ty,
    name: &str,
    params: &[KotlinType],
) -> Option<CallTarget> {
    let class = ty.non_null().obj_internal()?;
    let (package, classes) = builtin_class_scope(ty.non_null(), &class)?;
    let signature = callable_signature(
        DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &classes,
            native_interop_library: false,
        },
        &CallableShape {
            name,
            contexts: Vec::new(),
            receiver: None,
            params: params.iter().collect(),
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .ok()?;
    Some(CallTarget::Public(signature))
}

/// `kotlin.intArrayOf(vararg elements: Int)` and its siblings, which build a primitive array.
pub(super) fn primitive_array_factory(array: Ty) -> Option<CallTarget> {
    let class = array.obj_internal()?;
    if !class.package_matches("kotlin") || array.is_unsigned_array() {
        return None;
    }
    // Boundary conversion: the factory is named after its class (`IntArray` -> `intArrayOf`).
    let simple = class.segment_ref();
    let mut name = simple[..1].to_ascii_lowercase();
    name.push_str(&simple[1..]);
    name.push_str("Of");
    let parameter = KotlinType::class(class.render());
    top_level_kotlin_function(&name, &parameter, Vec::new())
}

/// `kotlin.arrayOf<T>(vararg elements: T)`.
pub(super) fn reference_array_factory() -> Option<CallTarget> {
    let id = crate::metadata::semantic::KotlinTypeParameterId(0);
    let element = KotlinType::Param {
        name: "T".to_owned(),
        id,
        nullable: false,
    };
    let parameter = KotlinType::Class {
        internal: "kotlin/Array".to_owned(),
        args: vec![KotlinType::OutProjection(Box::new(element))],
        nullable: false,
        shape: Default::default(),
    };
    top_level_kotlin_function(
        "arrayOf",
        &parameter,
        vec![TypeParameterShape {
            id,
            bounds: Vec::new(),
        }],
    )
}

pub(super) fn top_level_kotlin_function<'a>(
    name: &str,
    vararg: &'a KotlinType,
    type_parameters: Vec<TypeParameterShape<'a, KotlinType>>,
) -> Option<CallTarget> {
    let package = ["kotlin".to_owned()];
    let signature = callable_signature(
        DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        },
        &CallableShape {
            name,
            contexts: Vec::new(),
            receiver: None,
            params: vec![vararg],
            vararg: Some(0),
            type_parameters,
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .ok()?;
    Some(CallTarget::Public(signature))
}
