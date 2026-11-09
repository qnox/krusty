//! C functions and global variables as Kotlin/Native binds them.
//!
//! A function is an `external fun` calling the C symbol directly. A pointer parameter accepts any
//! `CValuesRef` of what it points at; a `const char *` parameter also gets an overload taking a
//! Kotlin `String`, which the pointer overload yields to. A variadic function takes its variable
//! arguments as `vararg variadicArguments: Any?`. A function the header defines (a `static
//! inline` helper) is declared, but its symbol names why it cannot be called.
//!
//! A global variable is a property reading and writing the C variable; a `const` one, an array,
//! or a struct is a `val`.

use super::super::model::{
    Declarations, Function as CFunction, IntRank, Qualifiers, Signedness, Type, TypeId, Variable,
};
use super::kotlin_type::{any, array, c_values_ref, string, KotlinType};
use super::mirror::{Mapper, Mirror, Unsupported};
use super::stub::{flags, Annotation, Function, Parameter, Property};

/// Where a function with a body defines it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Definition {
    Header,
    DefFile,
}

/// The function's overloads: one, or the `String` overload and the pointer one.
pub(super) fn function_stubs(
    mapper: &Mapper<'_>,
    function: &CFunction,
    definition: Option<Definition>,
) -> Result<Vec<Function>, Unsupported> {
    let declarations = mapper.declarations;
    let Type::Function(signature) = declarations.ty(function.ty) else {
        return Err(Unsupported);
    };
    let mut stable_names = true;
    let mut plain = Vec::new();
    let mut strings = Vec::new();
    // A parameter is mapped from its declared type: `int4 q` stays the typedef of an array.
    let params = if function.param_types.len() == signature.params.len() {
        &function.param_types
    } else {
        &signature.params
    };
    for (index, &param) in params.iter().enumerate() {
        let name = match function.param_names.get(index).cloned().flatten() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => {
                stable_names = false;
                format!("arg{index}")
            }
        };
        let ty = match values_ref(mapper, param)? {
            Some(ty) => ty.nullable(true),
            None => mapper.mirror(param)?.argument(),
        };
        let parameter = Parameter::new(&name, ty);
        strings.push(if is_c_string(declarations, param) {
            Some(Parameter {
                annotations: vec![Annotation::new(
                    "kotlinx/cinterop/internal/CCall.CString",
                    Vec::new(),
                )],
                ..Parameter::new(&name, string().nullable(true))
            })
        } else {
            None
        });
        plain.push(parameter);
    }
    if signature.variadic {
        plain.push(Parameter {
            vararg_element: Some(any().nullable(true)),
            ..Parameter::new("variadicArguments", array(any().nullable(true)))
        });
        strings.push(None);
    }
    let result = mapper.result(signature.result)?;
    let symbol = match definition {
        None => function
            .asm_name
            .as_deref()
            .unwrap_or(&function.name)
            .to_string(),
        Some(Definition::Header) => format!(
            "{} unsupported: function is defined in a header file",
            function.name
        ),
        Some(Definition::DefFile) => format!(
            "{} unsupported: function is defined in the .def file",
            function.name
        ),
    };
    let flags = if stable_names {
        flags::EXTERNAL_FUNCTION
    } else {
        flags::EXTERNAL_FUNCTION | flags::NON_STABLE_PARAMETER_NAMES
    };
    let stub = |parameters, low_priority: bool| {
        let mut annotations = vec![Annotation::direct(&symbol)];
        if low_priority {
            annotations.push(Annotation::new(
                "kotlin/internal/LowPriorityInOverloadResolution",
                Vec::new(),
            ));
        }
        annotations.push(Annotation::experimental());
        Function {
            name: function.name.to_string(),
            flags,
            result: result.clone(),
            parameters,
            annotations,
        }
    };
    if strings.iter().all(Option::is_none) {
        return Ok(vec![stub(plain, false)]);
    }
    let with_strings = plain
        .iter()
        .zip(strings)
        .map(|(parameter, string)| string.unwrap_or_else(|| parameter.clone()))
        .collect();
    Ok(vec![stub(with_strings, false), stub(plain, true)])
}

/// The global's property.
pub(super) fn global_property(
    mapper: &Mapper<'_>,
    variable: &Variable,
) -> Result<Property, Unsupported> {
    let declarations = mapper.declarations;
    let mirror = mapper.mirror(variable.ty)?;
    let symbol = variable.asm_name.as_deref().unwrap_or(&variable.name);
    let access = Annotation::new(
        "kotlinx/cinterop/internal/CGlobalAccess",
        vec![("name", super::stub::Value::String(symbol.to_string()))],
    );
    let pointer = Annotation::new(
        "kotlinx/cinterop/internal/CGlobalAccess.Pointer",
        Vec::new(),
    );
    let array = matches!(
        declarations.ty(declarations.canonical(variable.ty)),
        Type::Array { .. }
    );
    let (ty, var, getter_annotations) = match mirror {
        Mirror::ByValue { value, .. } if array => (value, false, vec![access.clone(), pointer]),
        Mirror::ByValue { .. } => (
            mirror.argument(),
            !is_const(declarations, variable.ty),
            vec![access.clone()],
        ),
        Mirror::ByRef { pointed } => (pointed, false, vec![access.clone(), pointer]),
    };
    Ok(Property {
        name: variable.name.to_string(),
        flags: if var {
            flags::GLOBAL_VAR
        } else {
            flags::GLOBAL_VAL
        },
        ty,
        getter_flags: flags::ANNOTATED_ACCESSOR,
        setter_flags: var.then_some(flags::ANNOTATED_ACCESSOR),
        annotations: vec![Annotation::experimental()],
        getter_annotations,
        setter_annotations: if var { vec![access] } else { Vec::new() },
        constant: None,
    })
}

/// What a pointer or array parameter accepts: `CValuesRef` of what it points at, `CValuesRef<*>`
/// for `void *`; `None` for any other parameter, and for a function pointer.
fn values_ref(mapper: &Mapper<'_>, ty: TypeId) -> Result<Option<KotlinType>, Unsupported> {
    let declarations = mapper.declarations;
    let pointee = match declarations.ty(without_qualifiers(declarations, ty)) {
        Type::Pointer(pointee)
        | Type::Array {
            element: pointee, ..
        } => *pointee,
        _ => return Ok(None),
    };
    Ok(match declarations.ty(declarations.canonical(pointee)) {
        Type::Void => Some(c_values_ref(None)),
        Type::Function(_) => None,
        Type::Array { .. } => values_ref(mapper, pointee)?,
        _ => Some(c_values_ref(Some(
            mapper.mirror(pointee)?.pointed().clone(),
        ))),
    })
}

/// Whether `ty` is a pointer to `const char`, which a Kotlin string can be passed as.
fn is_c_string(declarations: &Declarations, ty: TypeId) -> bool {
    let Type::Pointer(pointee) = declarations.ty(declarations.canonical(ty)) else {
        return false;
    };
    is_const(declarations, *pointee)
        && *declarations.ty(declarations.canonical(*pointee))
            == (Type::Int {
                rank: IntRank::Char,
                signedness: Signedness::Plain,
            })
}

/// Whether `ty` is `const`, directly or through the typedefs it names.
fn is_const(declarations: &Declarations, mut ty: TypeId) -> bool {
    loop {
        match declarations.ty(ty) {
            Type::Qualified { base, qualifiers } => {
                if qualifiers.contains(Qualifiers::CONST) {
                    return true;
                }
                ty = *base;
            }
            Type::Typedef(id) => ty = declarations.typedef(*id).ty,
            _ => return false,
        }
    }
}

fn without_qualifiers(declarations: &Declarations, mut ty: TypeId) -> TypeId {
    while let Type::Qualified { base, .. } = declarations.ty(ty) {
        ty = *base;
    }
    ty
}
