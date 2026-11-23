//! How kotlinc's `IrParcelSerializerFactory` writes one property to a `Parcel` and reads it back,
//! for the property types this port covers, and the `Parcel` members those serializers call.

use crate::ir::{ClassId, ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, PluginRuntime, RuntimeDeclaration, RuntimeFunction,
};
use crate::types::{type_name, Ty, TypeName};

/// The `Parcel` members one primitive or `String` value is written and read with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ParcelValue {
    String,
    Byte,
    Int,
    Long,
    Float,
    Double,
    /// Written as the `Int` 0 or 1.
    Boolean,
    /// Written as its `Int` widening.
    Short,
    /// Written as its `Int` code.
    Char,
    /// An unsigned integer, serialized as its signed counterpart of the same width.
    UByte,
    UShort,
    UInt,
    ULong,
    /// A specialized primitive array (`IntArray`, ...) of the given element, which `Parcel`
    /// writes and creates itself, `null` included.
    PrimitiveArray(Ty),
    /// An enum constant, stored as its `name`.
    Enum(TypeName),
    /// A final `@Parcelize` class of the same file, which kotlinc writes by calling its own
    /// `writeToParcel` and reads through its static `CREATOR`.
    Parcelize(TypeName),
    /// `android.os.Parcelable` itself, which `Parcel` writes and reads with the class's loader.
    Parcelable,
}

/// What the file shows kotlinc's factory about a classifier a property is typed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClassifierShape {
    Enum,
    /// A final `@Parcelize` class declared by this file.
    FileParcelize,
    Parcelable,
    Other,
}

impl ParcelValue {
    /// The value kotlinc's factory serializes a property of type `ty` as, and whether it wraps that
    /// serializer in its null-aware one; `None` for a type this port does not cover yet.
    pub(super) fn of(ty: Ty, shape: &dyn Fn(TypeName) -> ClassifierShape) -> Option<(Self, bool)> {
        let nullable = ty.is_nullable();
        let value = match ty.non_null() {
            Ty::String => Self::String,
            Ty::Byte => Self::Byte,
            Ty::Int => Self::Int,
            Ty::Long => Self::Long,
            Ty::Float => Self::Float,
            Ty::Double => Self::Double,
            Ty::Boolean => Self::Boolean,
            Ty::Short => Self::Short,
            Ty::Char => Self::Char,
            Ty::UByte => Self::UByte,
            Ty::UShort => Self::UShort,
            Ty::UInt => Self::UInt,
            Ty::ULong => Self::ULong,
            other => match other.nominal_classifier().map(|name| (name, shape(name))) {
                Some((name, ClassifierShape::Enum)) => Self::Enum(name),
                Some((name, ClassifierShape::FileParcelize)) => Self::Parcelize(name),
                Some((_, ClassifierShape::Parcelable)) => Self::Parcelable,
                _ => Self::PrimitiveArray(primitive_array_element(other)?),
            },
        };
        // `Parcel.writeString` and the array writers write `null` themselves, so kotlinc never
        // wraps a string or an array.
        let writes_null = matches!(
            value,
            Self::String | Self::PrimitiveArray(_) | Self::Parcelable
        );
        Some((value, nullable && !writes_null))
    }

    /// The signed value an unsigned one is serialized as; any other value is its own.
    fn signed(self) -> Self {
        match self {
            Self::UByte => Self::Byte,
            Self::UShort => Self::Short,
            Self::UInt => Self::Int,
            Self::ULong => Self::Long,
            other => other,
        }
    }

    /// The `Parcel` member kind that stores the value: a boolean, short or char goes through `Int`,
    /// an unsigned integer through its signed counterpart.
    fn member(self) -> Option<ParcelMember> {
        Some(match self.signed() {
            Self::String => ParcelMember::String,
            Self::Byte => ParcelMember::Byte,
            Self::Long => ParcelMember::Long,
            Self::Float => ParcelMember::Float,
            Self::Double => ParcelMember::Double,
            Self::PrimitiveArray(element) => ParcelMember::PrimitiveArray(element),
            Self::Enum(_) => ParcelMember::String,
            Self::Parcelable => ParcelMember::Parcelable,
            Self::Parcelize(_) => return None,
            _ => ParcelMember::Int,
        })
    }

    /// The Kotlin type of the value itself.
    fn source(self) -> Ty {
        match self {
            Self::Boolean => Ty::Boolean,
            Self::Short => Ty::Short,
            Self::Char => Ty::Char,
            Self::UByte => Ty::UByte,
            Self::UShort => Ty::UShort,
            Self::UInt => Ty::UInt,
            Self::ULong => Ty::ULong,
            Self::PrimitiveArray(element) => Ty::array(element),
            Self::Enum(name) | Self::Parcelize(name) => Ty::obj_name(name),
            Self::Parcelable => Ty::obj(PARCELABLE),
            Self::String => Ty::String,
            Self::Byte => Ty::Byte,
            Self::Int => Ty::Int,
            Self::Long => Ty::Long,
            Self::Float => Ty::Float,
            Self::Double => Ty::Double,
        }
    }
}

/// The `Parcel` member kinds values are stored with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParcelMember {
    Parcelable,
    String,
    Byte,
    Int,
    Long,
    Float,
    Double,
    PrimitiveArray(Ty),
}

/// The element of a `Parcel`-serialized specialized primitive array type, or `None` for any other
/// type. A `ShortArray` and the unsigned arrays are written by kotlinc's own element loops, so they
/// are not covered yet.
fn primitive_array_element(ty: Ty) -> Option<Ty> {
    let element = ty
        .array_elem()
        .filter(|_| ty.is_array() && !ty.is_reference_array())?;
    [
        Ty::Int,
        Ty::Byte,
        Ty::Long,
        Ty::Float,
        Ty::Double,
        Ty::Boolean,
        Ty::Char,
    ]
    .contains(&element)
    .then_some(element)
}

/// The `Parcel` members that write and create a primitive array of `element`.
fn array_names(element: Ty) -> (&'static str, &'static str) {
    match element {
        Ty::Int => ("writeIntArray", "createIntArray"),
        Ty::Byte => ("writeByteArray", "createByteArray"),
        Ty::Long => ("writeLongArray", "createLongArray"),
        Ty::Float => ("writeFloatArray", "createFloatArray"),
        Ty::Double => ("writeDoubleArray", "createDoubleArray"),
        Ty::Boolean => ("writeBooleanArray", "createBooleanArray"),
        Ty::Char => ("writeCharArray", "createCharArray"),
        other => unreachable!("{other:?} is not a Parcel-serialized primitive array element"),
    }
}

impl ParcelMember {
    /// The type `Parcel` declares the member with.
    fn declared(self) -> Ty {
        match self {
            Self::String => Ty::platform_nullable(Ty::String),
            Self::Byte => Ty::Byte,
            Self::Int => Ty::Int,
            Self::Long => Ty::Long,
            Self::Float => Ty::Float,
            Self::Double => Ty::Double,
            Self::PrimitiveArray(element) => Ty::platform_nullable(Ty::array(element)),
            Self::Parcelable => Ty::platform_nullable(Ty::obj(PARCELABLE)),
        }
    }

    /// The parameters `Parcel`'s writer for the member declares.
    fn write_params(self) -> Vec<Ty> {
        match self {
            Self::Parcelable => vec![self.declared(), Ty::Int],
            _ => vec![self.declared()],
        }
    }

    /// The parameters `Parcel`'s reader for the member declares.
    fn read_params(self) -> Vec<Ty> {
        match self {
            Self::Parcelable => vec![Ty::platform_nullable(Ty::obj(CLASS_LOADER))],
            _ => Vec::new(),
        }
    }

    /// The member kinds `used` values are stored with, in the order the members are bound, plus the
    /// `Int` a null-aware value's presence flag is stored with.
    fn bound(used: &[ParcelValue]) -> Vec<Self> {
        let wraps = used
            .iter()
            .any(|value| !matches!(value, ParcelValue::String | ParcelValue::PrimitiveArray(_)));
        let mut members = Vec::new();
        for member in [
            Self::String,
            Self::Byte,
            Self::Int,
            Self::Long,
            Self::Float,
            Self::Double,
        ] {
            if used.iter().any(|value| value.member() == Some(member))
                || (wraps && member == Self::Int)
            {
                members.push(member);
            }
        }
        for value in used {
            let Some(member) = value.member() else {
                continue;
            };
            if matches!(member, Self::PrimitiveArray(_) | Self::Parcelable)
                && !members.contains(&member)
            {
                members.push(member);
            }
        }
        members
    }

    fn write_name(self) -> &'static str {
        match self {
            Self::String => "writeString",
            Self::Byte => "writeByte",
            Self::Int => "writeInt",
            Self::Long => "writeLong",
            Self::Float => "writeFloat",
            Self::Double => "writeDouble",
            Self::PrimitiveArray(element) => array_names(element).0,
            Self::Parcelable => "writeParcelable",
        }
    }

    fn read_name(self) -> &'static str {
        match self {
            Self::String => "readString",
            Self::Byte => "readByte",
            Self::Int => "readInt",
            Self::Long => "readLong",
            Self::Float => "readFloat",
            Self::Double => "readDouble",
            Self::PrimitiveArray(element) => array_names(element).1,
            Self::Parcelable => "readParcelable",
        }
    }
}

const PARCELABLE: &str = "android/os/Parcelable";
const CLASS_LOADER: &str = "java/lang/ClassLoader";

/// A parcelized class of the file a `Parcelize` value calls: its own `writeToParcel`, which
/// `ParcelTarget::write_index` locates among its methods, and its static `CREATOR`.
#[derive(Clone, Copy, Debug)]
pub(super) struct ParcelTarget {
    pub(super) name: TypeName,
    pub(super) class: ClassId,
    pub(super) write_index: u32,
    pub(super) creator: u32,
}

/// Where one generated member writes or reads: the `Parcel` and `flags` values, the class it
/// belongs to, and the parcelized classes of the file.
#[derive(Clone, Copy)]
pub(super) struct ParcelContext<'a> {
    pub(super) parcel: u32,
    pub(super) flags: u32,
    pub(super) owner: TypeName,
    pub(super) targets: &'a [ParcelTarget],
}

impl ParcelContext<'_> {
    fn target(&self, name: TypeName) -> &ParcelTarget {
        self.targets
            .iter()
            .find(|target| target.name == name)
            .expect("a Parcelize value names a parcelized class of the file")
    }
}

/// One property's serializer: a value written directly, or kotlinc's `IrNullAwareParcelSerializer`
/// around one, which writes a presence flag before a non-null value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ParcelSerializer {
    pub(super) value: ParcelValue,
    pub(super) null_aware: bool,
}

/// The `Parcel` members a file's serializers call, bound once for the file.
pub(super) struct ParcelRuntime {
    /// `Parcelable.Creator.createFromParcel(Parcel)` and `newArray(Int)`, which the `Creator`
    /// class implements and bridges to.
    pub(super) create_from_parcel: BoundRuntimeFunction,
    pub(super) new_array: BoundRuntimeFunction,
    writers: Vec<(ParcelMember, BoundRuntimeFunction)>,
    readers: Vec<(ParcelMember, BoundRuntimeFunction)>,
    /// `Class.getClassLoader()`, which a `Parcelable` read passes `Parcel`.
    class_loader: Option<BoundRuntimeFunction>,
}

impl ParcelRuntime {
    /// Select the `Parcel` members for every value in `used`, or say which one the classpath lacks.
    pub(super) fn bind(
        runtime: &mut dyn PluginRuntime,
        used: &[ParcelValue],
    ) -> Result<Self, String> {
        let parcel = RuntimeDeclaration::Member(type_name(super::PARCEL));
        let creator = RuntimeDeclaration::Member(super::creator_interface());
        // The interface's `T`, as its Java members are normalized: erased to its bound.
        let element = Ty::platform_nullable(Ty::obj("kotlin/Any"));
        let create_from_parcel = runtime.function(&RuntimeFunction {
            declaration: creator,
            name: "createFromParcel",
            params: vec![Ty::platform_nullable(Ty::obj(super::PARCEL))],
            ret: element,
            appended: Vec::new(),
        })?;
        let new_array = runtime.function(&RuntimeFunction {
            declaration: creator,
            name: "newArray",
            params: vec![Ty::Int],
            ret: Ty::platform_nullable(Ty::array(element)),
            appended: Vec::new(),
        })?;
        let mut bound = Self {
            create_from_parcel,
            new_array,
            writers: Vec::new(),
            readers: Vec::new(),
            class_loader: None,
        };
        for value in ParcelMember::bound(used) {
            let writer = runtime.function(&RuntimeFunction {
                declaration: parcel,
                name: value.write_name(),
                params: value.write_params(),
                ret: Ty::Unit,
                appended: Vec::new(),
            })?;
            let reader = runtime.function(&RuntimeFunction {
                declaration: parcel,
                name: value.read_name(),
                params: value.read_params(),
                ret: value.declared(),
                appended: Vec::new(),
            })?;
            bound.writers.push((value, writer));
            bound.readers.push((value, reader));
        }
        if used.contains(&ParcelValue::Parcelable) {
            bound.class_loader = Some(runtime.function(&RuntimeFunction {
                declaration: RuntimeDeclaration::Member(type_name("java/lang/Class")),
                name: "getClassLoader",
                params: Vec::new(),
                ret: Ty::platform_nullable(Ty::obj(CLASS_LOADER)),
                appended: Vec::new(),
            })?);
        }
        Ok(bound)
    }

    fn writer(&self, value: ParcelMember) -> &BoundRuntimeFunction {
        &self
            .writers
            .iter()
            .find(|(bound, _)| *bound == value)
            .expect("every serialized value has a bound writer")
            .1
    }

    fn reader(&self, value: ParcelMember) -> &BoundRuntimeFunction {
        &self
            .readers
            .iter()
            .find(|(bound, _)| *bound == value)
            .expect("every serialized value has a bound reader")
            .1
    }

    /// Write `value`, a property of type `ty`, to the `Parcel` that value `parcel` holds.
    /// `temporary` is the value index the null-aware serializer keeps the value in.
    pub(super) fn write(
        &self,
        ir: &mut IrFile,
        serializer: ParcelSerializer,
        context: &ParcelContext<'_>,
        value: ExprId,
        ty: Ty,
        temporary: u32,
    ) -> ExprId {
        if !serializer.null_aware {
            return self.write_value(ir, serializer.value, context, value);
        }
        let parcel = context.parcel;
        // `irLetS(value) { irIfNull(it, writeInt(0), { writeInt(1); write(it) }) }`.
        let declaration = ir.add_expr(IrExpr::Variable {
            index: temporary,
            ty,
            init: Some(value),
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(temporary));
        let null = ir.add_expr(IrExpr::Const(IrConst::Null));
        let is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::RefEq,
            lhs: read,
            rhs: null,
        });
        let absent = self.write_flag(ir, parcel, 0);
        let present = self.write_flag(ir, parcel, 1);
        let stored = ir.add_expr(IrExpr::GetValue(temporary));
        let unboxed = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: stored,
            type_operand: serializer.value.source(),
        });
        let written = self.write_value(ir, serializer.value, context, unboxed);
        let otherwise = ir.add_expr(IrExpr::Block {
            stmts: vec![present, written],
            value: None,
        });
        let branch = ir.add_when(vec![(Some(is_null), absent), (None, otherwise)], Ty::Unit);
        ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, branch],
            value: None,
        })
    }

    /// Read a property of type `ty` from the `Parcel` that value `parcel` holds.
    pub(super) fn read(
        &self,
        ir: &mut IrFile,
        serializer: ParcelSerializer,
        context: &ParcelContext<'_>,
        ty: Ty,
    ) -> ExprId {
        if !serializer.null_aware {
            return self.read_value(ir, serializer.value, context);
        }
        // `if (parcel.readInt() == 0) null else read()`.
        let flag = self.read_value(ir, ParcelValue::Int, context);
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let absent = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: flag,
            rhs: zero,
        });
        let null = ir.add_expr(IrExpr::Const(IrConst::Null));
        let read = self.read_value(ir, serializer.value, context);
        // The member's result is the value itself, so boxing it to `ty` is the value's own box (an
        // unsigned integer's `box-impl`), not that of the signed type `Parcel` declares.
        ir.set_logical_type(read, serializer.value.source());
        let boxed = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: read,
            type_operand: ty,
        });
        ir.add_when(vec![(Some(absent), null), (None, boxed)], ty)
    }

    fn write_flag(&self, ir: &mut IrFile, parcel: u32, flag: i32) -> ExprId {
        let flag = ir.add_expr(IrExpr::Const(IrConst::Int(flag)));
        let context = ParcelContext {
            parcel,
            flags: 0,
            owner: type_name(PARCELABLE),
            targets: &[],
        };
        self.write_value(ir, ParcelValue::Int, &context, flag)
    }

    fn write_value(
        &self,
        ir: &mut IrFile,
        value: ParcelValue,
        context: &ParcelContext<'_>,
        arg: ExprId,
    ) -> ExprId {
        if let ParcelValue::Parcelize(name) = value {
            // `value.writeToParcel(dest, flags)`.
            let target = context.target(name);
            let dest = ir.add_expr(IrExpr::GetValue(context.parcel));
            let flags = ir.add_expr(IrExpr::GetValue(context.flags));
            return ir.add_expr(IrExpr::MethodCall {
                class: target.class,
                index: target.write_index,
                receiver: arg,
                args: vec![Some(dest), Some(flags)],
            });
        }
        let receiver = ir.add_expr(IrExpr::GetValue(context.parcel));
        let arg = match value.signed() {
            ParcelValue::Enum(_) => {
                // `value.name`, dispatched on the enum class itself as kotlinc does.
                ir.add_expr(IrExpr::Call {
                    callee: Box::new(crate::ir::Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::EnumName,
                        ret: Ty::String,
                    }),
                    dispatch_receiver: Some(arg),
                    args: Vec::new(),
                })
            }
            ParcelValue::Boolean => {
                // `if (value) 1 else 0`.
                let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
                let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                ir.add_when(vec![(Some(arg), one), (None, zero)], Ty::Int)
            }
            ParcelValue::Short | ParcelValue::Char => ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand: Ty::Int,
            }),
            _ => arg,
        };
        let member = value
            .member()
            .expect("a directly written value has a member");
        let args = match value {
            // `writeParcelable(value, flags)`.
            ParcelValue::Parcelable => {
                vec![arg, ir.add_expr(IrExpr::GetValue(context.flags))]
            }
            _ => vec![arg],
        };
        call(ir, self.writer(member), receiver, args)
    }

    fn read_value(
        &self,
        ir: &mut IrFile,
        value: ParcelValue,
        context: &ParcelContext<'_>,
    ) -> ExprId {
        let parcel = context.parcel;
        if let ParcelValue::Parcelize(name) = value {
            // `(C) C.CREATOR.createFromParcel(parcel)`.
            let creator = ir.add_expr(IrExpr::GetStatic(context.target(name).creator));
            let source = ir.add_expr(IrExpr::GetValue(parcel));
            let read = call(ir, &self.create_from_parcel, creator, vec![source]);
            return ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: read,
                type_operand: Ty::obj_name(name),
            });
        }
        let receiver = ir.add_expr(IrExpr::GetValue(parcel));
        let member = value.member().expect("a directly read value has a member");
        let args = if value == ParcelValue::Parcelable {
            // `readParcelable(Owner.class.getClassLoader())`.
            let owner = ir.add_expr(IrExpr::ClassConst {
                internal: Some(context.owner),
            });
            let loader = self
                .class_loader
                .as_ref()
                .expect("a Parcelable read binds Class.getClassLoader");
            vec![call(ir, loader, owner, Vec::new())]
        } else {
            Vec::new()
        };
        let read = call(ir, self.reader(member), receiver, args);
        match value.signed() {
            ParcelValue::Enum(classifier) => {
                // `Enum.valueOf(readString())`.
                ir.add_expr(IrExpr::EnumValueOf {
                    classifier,
                    arg: read,
                    declaration: crate::ir::EnumValueOfDeclaration::Member,
                })
            }
            ParcelValue::Boolean => {
                // `readInt() != 0`.
                let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Ne,
                    lhs: read,
                    rhs: zero,
                })
            }
            ParcelValue::Short | ParcelValue::Char => ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: read,
                type_operand: value.signed().source(),
            }),
            _ => read,
        }
    }
}

/// A call of the selected `Parcel` member `function` on `receiver`.
fn call(
    ir: &mut IrFile,
    function: &BoundRuntimeFunction,
    receiver: ExprId,
    args: Vec<ExprId>,
) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Box::new(crate::ir::Callee::External {
            target: function.target,
            default_provider: None,
            params: function.params.clone(),
            ret: function.ret,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: Some(receiver),
        args,
    })
}
