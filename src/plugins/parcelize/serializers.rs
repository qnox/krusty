//! How kotlinc's `IrParcelSerializerFactory` writes one property to a `Parcel` and reads it back,
//! for the property types this port covers, and the `Parcel` members those serializers call.

use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, PluginRuntime, RuntimeDeclaration, RuntimeFunction,
};
use crate::types::{type_name, Ty};

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
}

impl ParcelValue {
    /// The value kotlinc's factory serializes a property of type `ty` as, and whether it wraps that
    /// serializer in its null-aware one; `None` for a type this port does not cover yet.
    pub(super) fn of(ty: Ty) -> Option<(Self, bool)> {
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
            _ => return None,
        };
        // `Parcel.writeString` writes `null` itself, so kotlinc never wraps a string.
        Some((value, nullable && value != Self::String))
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
    fn member(self) -> ParcelMember {
        match self.signed() {
            Self::String => ParcelMember::String,
            Self::Byte => ParcelMember::Byte,
            Self::Long => ParcelMember::Long,
            Self::Float => ParcelMember::Float,
            Self::Double => ParcelMember::Double,
            _ => ParcelMember::Int,
        }
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
    String,
    Byte,
    Int,
    Long,
    Float,
    Double,
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
        }
    }

    /// Every member kind, in the order the members are bound.
    const ALL: [Self; 6] = [
        Self::String,
        Self::Byte,
        Self::Int,
        Self::Long,
        Self::Float,
        Self::Double,
    ];

    fn write_name(self) -> &'static str {
        match self {
            Self::String => "writeString",
            Self::Byte => "writeByte",
            Self::Int => "writeInt",
            Self::Long => "writeLong",
            Self::Float => "writeFloat",
            Self::Double => "writeDouble",
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
        }
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
        };
        // A null-aware value writes and reads its presence flag as an `Int`.
        let wraps = used.iter().any(|&value| value != ParcelValue::String);
        for value in ParcelMember::ALL {
            if !used.iter().any(|used| used.member() == value)
                && !(wraps && value == ParcelMember::Int)
            {
                continue;
            }
            let writer = runtime.function(&RuntimeFunction {
                declaration: parcel,
                name: value.write_name(),
                params: vec![value.declared()],
                ret: Ty::Unit,
                appended: Vec::new(),
            })?;
            let reader = runtime.function(&RuntimeFunction {
                declaration: parcel,
                name: value.read_name(),
                params: Vec::new(),
                ret: value.declared(),
                appended: Vec::new(),
            })?;
            bound.writers.push((value, writer));
            bound.readers.push((value, reader));
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
        parcel: u32,
        value: ExprId,
        ty: Ty,
        temporary: u32,
    ) -> ExprId {
        if !serializer.null_aware {
            return self.write_value(ir, serializer.value, parcel, value);
        }
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
        let written = self.write_value(ir, serializer.value, parcel, unboxed);
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
        parcel: u32,
        ty: Ty,
    ) -> ExprId {
        if !serializer.null_aware {
            return self.read_value(ir, serializer.value, parcel);
        }
        // `if (parcel.readInt() == 0) null else read()`.
        let flag = self.read_value(ir, ParcelValue::Int, parcel);
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let absent = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: flag,
            rhs: zero,
        });
        let null = ir.add_expr(IrExpr::Const(IrConst::Null));
        let read = self.read_value(ir, serializer.value, parcel);
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
        self.write_value(ir, ParcelValue::Int, parcel, flag)
    }

    fn write_value(&self, ir: &mut IrFile, value: ParcelValue, parcel: u32, arg: ExprId) -> ExprId {
        let receiver = ir.add_expr(IrExpr::GetValue(parcel));
        let arg = match value.signed() {
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
        call(ir, self.writer(value.member()), receiver, vec![arg])
    }

    fn read_value(&self, ir: &mut IrFile, value: ParcelValue, parcel: u32) -> ExprId {
        let receiver = ir.add_expr(IrExpr::GetValue(parcel));
        let read = call(ir, self.reader(value.member()), receiver, Vec::new());
        match value.signed() {
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
