//! Reference native plugin — `kotlinx.serialization`. See `docs/PLUGIN_API.md`.
//!
//! For a `@Serializable class Foo(val a: Int, val b: String)`, kotlinc's serialization plugin
//! synthesizes (across its FIR + IR backend extensions):
//!
//!   - a nested `Foo.$serializer` **object** implementing `kotlinx/serialization/KSerializer<Foo>`
//!     with `getDescriptor`, `serialize`, `deserialize`, `childSerializers`;
//!   - `Foo.serializer()` returning that `KSerializer` (kotlinc puts it on `Foo.Companion`).
//!
//! Phase split mirrors kotlinc:
//!   - `generate_declarations` (FIR decl-gen) — the `$serializer` object, its members' signatures,
//!     and the `serializer()` accessor whose body reads the `$serializer` singleton.
//!   - `transform_bodies` (IR backend) — fills `childSerializers` with a REAL array of one element
//!     serializer per primary-constructor property (so its arity genuinely tracks the field list),
//!     and `serialize`/`deserialize` with placeholder bodies. In production those two call the real
//!     published `kotlinx-serialization-core` runtime (`Encoder`/`Decoder`/`SerialDescriptor`); the
//!     PoC keeps them as `return` so the surface — not the runtime — is what is under test.

use crate::ir::{Callee, ExprId, IrConst, IrExpr, IrFile, IrFunction, IrType, IrTypeOp};
use crate::plugins::{synthetic_class, IrPlugin, PluginContext};

pub const SERIALIZABLE_FQ: &str = "kotlinx/serialization/Serializable";
pub const KSERIALIZER_FQ: &str = "kotlinx/serialization/KSerializer";

/// The `kotlinx.serialization` runtime ABI the generated code must match. The synthesized member
/// shape changed across releases, so the plugin emits *per target version* — exactly as krusty
/// itself is pinned to a kotlinc version. A mismatch between generated code and the linked runtime
/// is a runtime linkage error, so this is not cosmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SerializationAbi {
    /// core < 1.6: the per-class write helper is the unmangled `write$Self`.
    V1_0,
    /// core >= 1.6 (Kotlin >= 1.8.20): the helper is module-mangled `write$Self$<module>` to avoid
    /// cross-module name clashes.
    #[default]
    V1_6Plus,
}

impl SerializationAbi {
    /// Pick the ABI from a `kotlinx-serialization-core` version string (`"1.8.1"`, `"1.5.0"`). As a
    /// kotlinc drop-in, krusty derives this from the runtime jar on `-classpath` — NOT a krusty flag —
    /// so the same inputs kotlinc gets select the same codegen. `< 1.6` → `V1_0`, else `V1_6Plus`.
    pub fn from_core_version(version: &str) -> SerializationAbi {
        let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(0);
        if (major, minor) < (1, 6) {
            SerializationAbi::V1_0
        } else {
            SerializationAbi::V1_6Plus
        }
    }

    /// Detect the ABI from a `-classpath` jar list by finding the `kotlinx-serialization-core[-jvm]`
    /// jar and reading its version. Returns `None` if the runtime isn't on the classpath (then the
    /// `@Serializable` annotation itself wouldn't resolve — a user error kotlinc also reports).
    pub fn from_classpath(cp_jars: &[String]) -> Option<SerializationAbi> {
        cp_jars
            .iter()
            .filter_map(|j| {
                let name = j.rsplit('/').next().unwrap_or(j);
                let stem = name.strip_suffix(".jar")?;
                // kotlinx-serialization-core-jvm-1.8.1  /  kotlinx-serialization-core-1.8.1
                let rest = stem.strip_prefix("kotlinx-serialization-core")?;
                let rest = rest.strip_prefix("-jvm").unwrap_or(rest);
                let ver = rest.strip_prefix('-')?;
                Some(SerializationAbi::from_core_version(ver))
            })
            .next()
    }
}

/// The serialization extension, pinned to a target runtime ABI + the compilation's module name
/// (needed for the >=1.6 `write$Self$<module>` mangling).
pub struct SerializationPlugin {
    pub abi: SerializationAbi,
    pub module: String,
}

impl SerializationPlugin {
    pub fn new(abi: SerializationAbi, module: impl Into<String>) -> Self {
        Self {
            abi,
            module: module.into(),
        }
    }

    /// The per-class write-helper name for the target ABI.
    fn write_self_name(&self) -> String {
        match self.abi {
            SerializationAbi::V1_0 => "write$Self".to_string(),
            SerializationAbi::V1_6Plus => format!("write$Self${}", self.module),
        }
    }
}

impl Default for SerializationPlugin {
    fn default() -> Self {
        Self::new(SerializationAbi::default(), "main")
    }
}

/// The FqName of the synthesized serializer object for a `@Serializable` class (`Foo` → `Foo$serializer`).
fn serializer_fq(class_fq: &str) -> String {
    format!("{class_fq}$serializer")
}

fn unit() -> IrType {
    IrType::Unit
}

fn class_ty(fq: &str) -> IrType {
    IrType::Class {
        fq_name: fq.to_string(),
        type_args: Vec::new(),
        nullable: false,
    }
}

/// The JVM getter name for a Kotlin property (`a` → `getA`).
fn getter_name(prop: &str) -> String {
    let mut c = prop.chars();
    match c.next() {
        Some(f) => format!("get{}{}", f.to_uppercase(), c.as_str()),
        None => "get".to_string(),
    }
}

/// The boxed reference descriptor for a primitive fq name, or `None` if it isn't a primitive.
fn boxed_descriptor(fq: &str) -> Option<&'static str> {
    Some(match fq {
        "kotlin/Int" => "Ljava/lang/Integer;",
        "kotlin/Long" => "Ljava/lang/Long;",
        "kotlin/Boolean" => "Ljava/lang/Boolean;",
        "kotlin/Double" => "Ljava/lang/Double;",
        "kotlin/Float" => "Ljava/lang/Float;",
        "kotlin/Char" => "Ljava/lang/Character;",
        "kotlin/Byte" => "Ljava/lang/Byte;",
        "kotlin/Short" => "Ljava/lang/Short;",
        _ => return None,
    })
}

/// The JVM descriptor for a property type (just what `serialize`'s getter calls need). A nullable
/// primitive is carried as its boxed type (`Int?` → `Ljava/lang/Integer;`), matching the getter krusty
/// emits for a nullable primitive property.
fn ty_descriptor(ty: &IrType) -> String {
    let (fq, nullable) = match ty {
        IrType::Class {
            fq_name, nullable, ..
        } => (fq_name.as_str(), *nullable),
        _ => ("kotlin/Any", false),
    };
    if nullable {
        if let Some(boxed) = boxed_descriptor(fq) {
            return boxed.to_string();
        }
    }
    match fq {
        "kotlin/Int" => "I".to_string(),
        "kotlin/Long" => "J".to_string(),
        "kotlin/Boolean" => "Z".to_string(),
        "kotlin/Double" => "D".to_string(),
        "kotlin/Float" => "F".to_string(),
        "kotlin/String" => "Ljava/lang/String;".to_string(),
        other => format!("L{other};"),
    }
}

/// The `CompositeDecoder.decode<T>Element` method + descriptor for a property type, or `None` for a
/// reference/richer type (which needs `decodeSerializableElement`). Covers the full primitive set +
/// String; Long/Double are 2-slot and their field locals are sized via `slot_width`.
fn decode_element_method(ty: &IrType) -> Option<(&'static str, &'static str)> {
    let fq = match ty {
        IrType::Class { fq_name, .. } => fq_name.as_str(),
        _ => return None,
    };
    Some(match fq {
        "kotlin/Int" => (
            "decodeIntElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)I",
        ),
        "kotlin/Long" => (
            "decodeLongElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)J",
        ),
        "kotlin/Boolean" => (
            "decodeBooleanElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)Z",
        ),
        "kotlin/Float" => (
            "decodeFloatElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)F",
        ),
        "kotlin/Double" => (
            "decodeDoubleElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)D",
        ),
        "kotlin/String" => (
            "decodeStringElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)Ljava/lang/String;",
        ),
        _ => return None, // reference/richer types: decodeSerializableElement (future work)
    })
}

/// JVM local-slot width of a field type. Long/Double take two slots — but a NULLABLE Long?/Double? is
/// carried boxed (`java.lang.Long`/`Double`), a one-slot reference.
fn slot_width(ty: &IrType) -> u32 {
    match ty {
        IrType::Class {
            fq_name,
            nullable: false,
            ..
        } if fq_name == "kotlin/Long" || fq_name == "kotlin/Double" => 2,
        _ => 1,
    }
}

/// The default value for a field's local before the decode loop fills it.
fn default_const(ty: &IrType) -> IrConst {
    match ty {
        IrType::Class { fq_name, .. } if fq_name == "kotlin/Int" => IrConst::Int(0),
        IrType::Class { fq_name, .. } if fq_name == "kotlin/Long" => IrConst::Long(0),
        IrType::Class { fq_name, .. } if fq_name == "kotlin/Boolean" => IrConst::Boolean(false),
        IrType::Class { fq_name, .. } if fq_name == "kotlin/Float" => IrConst::Float(0.0),
        IrType::Class { fq_name, .. } if fq_name == "kotlin/Double" => IrConst::Double(0.0),
        IrType::Class { fq_name, .. } if fq_name == "kotlin/String" => {
            IrConst::String(String::new())
        }
        _ => IrConst::Null,
    }
}

/// An `invokeinterface` callee on a runtime interface (`Encoder`/`CompositeEncoder`/…).
fn virtual_iface(owner: &str, name: &str, descriptor: &str) -> Callee {
    Callee::Virtual {
        owner: owner.to_string(),
        name: name.to_string(),
        descriptor: descriptor.to_string(),
        interface: true,
    }
}

/// The `CompositeEncoder.encode<T>Element` method + descriptor for a property type, or `None` if the
/// type isn't a directly-encodable primitive/String (a richer type needs `encodeSerializableElement`).
fn encode_element_method(ty: &IrType) -> Option<(&'static str, &'static str)> {
    let fq = match ty {
        IrType::Class { fq_name, .. } => fq_name.as_str(),
        _ => return None,
    };
    let d = "Lkotlinx/serialization/descriptors/SerialDescriptor;";
    Some(match fq {
        "kotlin/Int" => (
            "encodeIntElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;II)V",
        ),
        "kotlin/Long" => (
            "encodeLongElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;IJ)V",
        ),
        "kotlin/Boolean" => (
            "encodeBooleanElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;IZ)V",
        ),
        "kotlin/Double" => (
            "encodeDoubleElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ID)V",
        ),
        "kotlin/Float" => (
            "encodeFloatElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;IF)V",
        ),
        "kotlin/String" => (
            "encodeStringElement",
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILjava/lang/String;)V",
        ),
        _ => {
            let _ = d;
            return None;
        }
    })
}

fn kserializer_of(arg: IrType) -> IrType {
    IrType::Class {
        fq_name: KSERIALIZER_FQ.to_string(),
        type_args: vec![arg],
        nullable: false,
    }
}

fn is_nullable(ty: &IrType) -> bool {
    matches!(ty, IrType::Class { nullable: true, .. })
}

/// The internal name of the kotlinx builtin `KSerializer` singleton (`…INSTANCE`) for a directly
/// serializable element type — used as the element serializer for a NULLABLE property, which goes
/// through `encode/decodeNullableSerializableElement` (there is no `encodeNullable<Prim>Element`).
///
/// Covers `String` (reference) and the primitive set. For a nullable primitive the property's getter
/// and field are the boxed type (`Int?` → `getX()Ljava/lang/Integer;`, verified what krusty emits), so
/// the value reaching `encode/decodeNullableSerializableElement(…, Object)` is already a reference — no
/// extra autoboxing. The serializer singleton itself serializes the unboxed primitive.
fn builtin_element_serializer(ty: &IrType) -> Option<&'static str> {
    let fq = match ty {
        IrType::Class { fq_name, .. } => fq_name.as_str(),
        _ => return None,
    };
    // A nullable primitive is lowered to its BOXED fq name (`Int?` → `java/lang/Integer`), so match
    // both the Kotlin primitive name and the boxed name.
    Some(match fq {
        "kotlin/String" | "java/lang/String" => "kotlinx/serialization/internal/StringSerializer",
        "kotlin/Int" | "java/lang/Integer" => "kotlinx/serialization/internal/IntSerializer",
        "kotlin/Long" | "java/lang/Long" => "kotlinx/serialization/internal/LongSerializer",
        "kotlin/Boolean" | "java/lang/Boolean" => {
            "kotlinx/serialization/internal/BooleanSerializer"
        }
        "kotlin/Double" | "java/lang/Double" => "kotlinx/serialization/internal/DoubleSerializer",
        "kotlin/Float" | "java/lang/Float" => "kotlinx/serialization/internal/FloatSerializer",
        "kotlin/Char" | "java/lang/Character" => "kotlinx/serialization/internal/CharSerializer",
        "kotlin/Byte" | "java/lang/Byte" => "kotlinx/serialization/internal/ByteSerializer",
        "kotlin/Short" | "java/lang/Short" => "kotlinx/serialization/internal/ShortSerializer",
        _ => return None,
    })
}

impl SerializationPlugin {
    /// Add `static serializer(): KSerializer<C>` returning a fresh instance of the explicit serializer
    /// `X` from `@Serializable(with = X::class)`: `new X(Reflection.getOrCreateKotlinClass(C.class))`.
    /// `ContextualSerializer`/`PolymorphicSerializer` (any serializer with a single `KClass` ctor) take
    /// the serialized class's `KClass`; their descriptors carry the right `SerialKind`.
    fn add_custom_serializer_accessor(
        ir: &mut IrFile,
        class_id: u32,
        class_fq: &str,
        custom: &str,
    ) {
        let classlit = ir.add_expr(IrExpr::ClassConst {
            internal: class_fq.to_string(),
        });
        let kclass = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: "kotlin/jvm/internal/Reflection".to_string(),
                name: "getOrCreateKotlinClass".to_string(),
                descriptor: "(Ljava/lang/Class;)Lkotlin/reflect/KClass;".to_string(),
                inline: false,
                must_inline: false,
            },
            dispatch_receiver: None,
            args: vec![classlit],
        });
        let inst = ir.add_expr(IrExpr::NewExternal {
            internal: custom.to_string(),
            ctor_desc: "(Lkotlin/reflect/KClass;)V".to_string(),
            args: vec![kclass],
        });
        let ret = ir.add_expr(IrExpr::Return(Some(inst)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        let accessor = ir.add_fun(IrFunction {
            name: "serializer".to_string(),
            params: vec![],
            ret: kserializer_of(class_ty(class_fq)),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.classes[class_id as usize].methods.push(accessor);
    }

    /// Add a method to `ir` and return its `FunId`.
    fn add_method(
        ir: &mut IrFile,
        owner_fq: &str,
        name: &str,
        params: Vec<IrType>,
        ret: IrType,
        body: Option<ExprId>,
    ) -> u32 {
        ir.add_fun(IrFunction {
            name: name.to_string(),
            params,
            ret,
            body,
            is_static: false,
            dispatch_receiver: Some(owner_fq.to_string()),
            param_checks: Vec::new(),
        })
    }
}

impl IrPlugin for SerializationPlugin {
    fn name(&self) -> &str {
        "kotlinx.serialization"
    }

    /// FIR declaration generation: the `$serializer` object + members + the `serializer()` accessor.
    /// PRODUCTION NOTE: hosted at the signature phase so `Foo.serializer()` in user code resolves.
    fn generate_declarations(&self, ir: &mut IrFile, ctx: &PluginContext) {
        for class_id in ctx.classes_with_simple("Serializable") {
            let class_fq = ir.classes[class_id as usize].fq_name.clone();
            // `@Serializable(with = X::class)`: no generated `$serializer` — `serializer()` returns an
            // instance of the explicit serializer `X` (`new X(C::class)`), the way kotlinc compiles it.
            if let Some(custom) = ir.classes[class_id as usize].custom_serializer.clone() {
                Self::add_custom_serializer_accessor(ir, class_id, &class_fq, &custom);
                continue;
            }
            let ser_fq = serializer_fq(&class_fq);

            // Member signatures; bodies are filled in `transform_bodies`.
            let descriptor = Self::add_method(
                ir,
                &ser_fq,
                "getDescriptor",
                vec![],
                class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                None,
            );
            let serialize = Self::add_method(
                ir,
                &ser_fq,
                "serialize",
                vec![
                    class_ty("kotlinx/serialization/encoding/Encoder"),
                    class_ty(&class_fq),
                ],
                unit(),
                None,
            );
            let deserialize = Self::add_method(
                ir,
                &ser_fq,
                "deserialize",
                vec![class_ty("kotlinx/serialization/encoding/Decoder")],
                class_ty(&class_fq),
                None,
            );
            let child = Self::add_method(
                ir,
                &ser_fq,
                "childSerializers",
                vec![],
                IrType::Class {
                    fq_name: "kotlin/Array".to_string(),
                    type_args: vec![kserializer_of(class_ty("kotlin/Any"))],
                    nullable: false,
                },
                None,
            );

            let foo_fields: Vec<(String, IrType)> = ir.classes[class_id as usize].fields.clone();
            // `@SerialName("x")` overrides a property's descriptor element name (and thus its JSON key).
            let serial_names: Vec<(String, String)> =
                ir.classes[class_id as usize].serial_names.clone();
            let element_name = |prop: &str| -> String {
                serial_names
                    .iter()
                    .find(|(p, _)| p == prop)
                    .map(|(_, n)| n.clone())
                    .unwrap_or_else(|| prop.to_string())
            };

            let mut ser = synthetic_class(&ser_fq);
            ser.is_object = true; // `$serializer` is a singleton object (INSTANCE)
            ser.interfaces = vec![KSERIALIZER_FQ.to_string()];
            ser.supertypes = vec![kserializer_of(class_ty(&class_fq))];
            // A `descriptor` field (a `PluginGeneratedSerialDescriptor`), built in the object's <init>.
            ser.fields = vec![(
                "descriptor".to_string(),
                class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
            )];
            ser.field_final = vec![true];
            ser.field_private = vec![true];
            ser.ctor_param_count = 0;
            ser.methods = vec![descriptor, serialize, deserialize, child];
            // Erased generic bridges the `KSerializer<Foo>` interface requires: the JVM sees
            // `serialize(Encoder, Object)` / `deserialize(Decoder): Object`; each adapts args/return
            // and delegates to the concrete `Foo`-typed override.
            ser.bridges = vec![
                crate::ir::Bridge {
                    name: "serialize".to_string(),
                    erased_params: vec![
                        class_ty("kotlinx/serialization/encoding/Encoder"),
                        class_ty("kotlin/Any"),
                    ],
                    erased_ret: unit(),
                    concrete_params: vec![
                        class_ty("kotlinx/serialization/encoding/Encoder"),
                        class_ty(&class_fq),
                    ],
                    concrete_ret: unit(),
                    target_name: None,
                    box_ret: None,
                    unbox_params: Vec::new(),
                },
                crate::ir::Bridge {
                    name: "deserialize".to_string(),
                    erased_params: vec![class_ty("kotlinx/serialization/encoding/Decoder")],
                    erased_ret: class_ty("kotlin/Any"),
                    concrete_params: vec![class_ty("kotlinx/serialization/encoding/Decoder")],
                    concrete_ret: class_ty(&class_fq),
                    target_name: None,
                    box_ret: None,
                    unbox_params: Vec::new(),
                },
            ];
            let ser_id = ir.add_class(ser);

            // Build the `descriptor` field in <init>: `descriptor = PluginGeneratedSerialDescriptor(
            // "<fqname>", null, <n>)` then `descriptor.addElement("<prop>", false)` per property.
            // Build in a local typed as PluginGeneratedSerialDescriptor (so `addElement` —
            // invokevirtual on PGSD — has a correctly-typed receiver), then store to the field:
            //   val d = PluginGeneratedSerialDescriptor("<fq>", null, n)   [local 1, this=0]
            //   d.addElement("<prop>", false) ...
            //   this.descriptor = d
            let pgsd_internal = "kotlinx/serialization/internal/PluginGeneratedSerialDescriptor";
            let pgsd_name = ir.add_expr(IrExpr::Const(IrConst::String(class_fq.replace('/', "."))));
            let pgsd_null = ir.add_expr(IrExpr::Const(IrConst::Null));
            let pgsd_n = ir.add_expr(IrExpr::Const(IrConst::Int(foo_fields.len() as i32)));
            let pgsd = ir.add_expr(IrExpr::NewExternal {
                internal: pgsd_internal.to_string(),
                ctor_desc:
                    "(Ljava/lang/String;Lkotlinx/serialization/internal/GeneratedSerializer;I)V"
                        .to_string(),
                args: vec![pgsd_name, pgsd_null, pgsd_n],
            });
            let dvar = ir.add_expr(IrExpr::Variable {
                index: 1,
                ty: class_ty(pgsd_internal),
                init: Some(pgsd),
            });
            let mut init_stmts = vec![dvar];
            for (pname, _) in &foo_fields {
                let d = ir.add_expr(IrExpr::GetValue(1));
                let nm = ir.add_expr(IrExpr::Const(IrConst::String(element_name(pname))));
                let opt = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
                init_stmts.push(ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner: pgsd_internal.to_string(),
                        name: "addElement".to_string(),
                        descriptor: "(Ljava/lang/String;Z)V".to_string(),
                        interface: false,
                    },
                    dispatch_receiver: Some(d),
                    args: vec![nm, opt],
                }));
            }
            let this0 = ir.add_expr(IrExpr::GetValue(0));
            let dval = ir.add_expr(IrExpr::GetValue(1));
            init_stmts.push(ir.add_expr(IrExpr::SetField {
                receiver: this0,
                class: ser_id,
                index: 0,
                value: dval,
            }));
            let init = ir.add_expr(IrExpr::Block {
                stmts: init_stmts,
                value: None,
            });
            let ser_idx = ser_id as usize;
            ir.classes[ser_idx].init_body = Some(init);

            // `serializer()` accessor — body reads the `$serializer` singleton (`Foo$serializer.INSTANCE`).
            // kotlinc emits it on `Foo.Companion`; the PoC attaches it to the class as a static member.
            let inst = ir.add_expr(IrExpr::StaticInstance {
                owner: ser_id,
                ty: ser_id,
                field: "INSTANCE",
            });
            let inst_ret = ir.add_expr(IrExpr::Return(Some(inst)));
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![inst_ret],
                value: None,
            });
            let accessor = ir.add_fun(IrFunction {
                name: "serializer".to_string(),
                params: vec![],
                ret: kserializer_of(class_ty(&class_fq)),
                body: Some(body),
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
            ir.classes[class_id as usize].methods.push(accessor);

            // `write$Self` helper — its NAME is ABI-version-dependent (mangled with the module name
            // on core >= 1.6). Emitted on the serialized class as a static member with a concrete
            // (no-op) body so the class stays concrete (an empty-body method would force it abstract).
            let ws_ret = ir.add_expr(IrExpr::Return(None));
            let ws_body = ir.add_expr(IrExpr::Block {
                stmts: vec![ws_ret],
                value: None,
            });
            let write_self = ir.add_fun(IrFunction {
                name: self.write_self_name(),
                params: vec![
                    class_ty(&class_fq),
                    class_ty("kotlinx/serialization/encoding/CompositeEncoder"),
                    class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                ],
                ret: unit(),
                body: Some(ws_body),
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
            ir.classes[class_id as usize].methods.push(write_self);
        }
    }

    /// IR backend generation: fill `childSerializers` with a real per-field element-serializer array
    /// (arity == field count), and `serialize`/`deserialize` with placeholder `return` bodies.
    fn transform_bodies(&self, ir: &mut IrFile, ctx: &PluginContext) {
        for class_id in ctx.classes_with_simple("Serializable") {
            // `@Serializable(with=X)` classes have no generated `$serializer` to fill (handled wholly in
            // `generate_declarations`).
            if ir.classes[class_id as usize].custom_serializer.is_some() {
                continue;
            }
            let fields: Vec<(String, IrType)> = ir.classes[class_id as usize].fields.clone();
            let field_types: Vec<IrType> = fields.iter().map(|(_, ty)| ty.clone()).collect();
            let class_internal = ir.classes[class_id as usize].fq_name.clone();
            let ser_fq = serializer_fq(&ir.classes[class_id as usize].fq_name);
            let Some(ser_idx) = ir.classes.iter().position(|c| c.fq_name == ser_fq) else {
                continue;
            };
            // The serialized class's ClassId (for constructing it in `deserialize`).
            let foo_id = class_id;
            // For each field: the ClassId of its element `$serializer` if the field's type is itself a
            // `@Serializable` class krusty generated a serializer for (nested/composite), else None.
            let nested: Vec<Option<u32>> = fields
                .iter()
                .map(|(_, ty)| match ty {
                    IrType::Class { fq_name, .. } => {
                        let s = serializer_fq(fq_name);
                        ir.classes
                            .iter()
                            .position(|c| c.fq_name == s)
                            .map(|i| i as u32)
                    }
                    _ => None,
                })
                .collect();
            for fid in ir.classes[ser_idx].methods.clone() {
                match ir.functions[fid as usize].name.as_str() {
                    "getDescriptor" => {
                        // return this.descriptor
                        let recv = ir.add_expr(IrExpr::GetValue(0));
                        let d = ir.add_expr(IrExpr::GetField {
                            receiver: recv,
                            class: ser_idx as u32,
                            index: 0,
                        });
                        let ret = ir.add_expr(IrExpr::Return(Some(d)));
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![ret],
                            value: None,
                        });
                        ir.functions[fid as usize].body = Some(body);
                    }
                    "serialize" => {
                        // serialize(encoder=1, value=2): drive the CompositeEncoder per property.
                        //   val c = encoder.beginStructure(descriptor)        [local 3]
                        //   c.encode<T>Element(descriptor, i, value.<prop_i>)
                        //   c.endStructure(descriptor)
                        let ser_cid = ser_idx as u32;
                        let this_desc = |ir: &mut IrFile| -> ExprId {
                            let r = ir.add_expr(IrExpr::GetValue(0));
                            ir.add_expr(IrExpr::GetField {
                                receiver: r,
                                class: ser_cid,
                                index: 0,
                            })
                        };
                        let enc = ir.add_expr(IrExpr::GetValue(1));
                        let d0 = this_desc(ir);
                        let begin = ir.add_expr(IrExpr::Call {
                            callee: virtual_iface(
                                "kotlinx/serialization/encoding/Encoder",
                                "beginStructure",
                                "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/CompositeEncoder;",
                            ),
                            dispatch_receiver: Some(enc),
                            args: vec![d0],
                        });
                        let cvar = ir.add_expr(IrExpr::Variable {
                            index: 3,
                            ty: class_ty("kotlinx/serialization/encoding/CompositeEncoder"),
                            init: Some(begin),
                        });
                        let mut stmts = vec![cvar];
                        let mut bail = false;
                        for (i, (pname, ty)) in fields.iter().enumerate() {
                            let d = this_desc(ir);
                            let idx = ir.add_expr(IrExpr::Const(IrConst::Int(i as i32)));
                            // Read the property via its PUBLIC getter (`value.getX()`) — the backing
                            // field is private, so a separate `$serializer` class can't read it directly.
                            let vrecv = ir.add_expr(IrExpr::GetValue(2));
                            let v = ir.add_expr(IrExpr::Call {
                                callee: Callee::Virtual {
                                    owner: class_internal.clone(),
                                    name: getter_name(pname),
                                    descriptor: format!("(){}", ty_descriptor(ty)),
                                    interface: false,
                                },
                                dispatch_receiver: Some(vrecv),
                                args: vec![],
                            });
                            let c = ir.add_expr(IrExpr::GetValue(3));
                            if let Some(nsid) = nested[i] {
                                // Nested @Serializable: encode[Nullable]SerializableElement(desc, i,
                                // <T>$serializer.INSTANCE, value.getX()). The nullable variant has the
                                // SAME descriptor — it just writes JSON null when the value is null — so
                                // it's a method-name swap for an `Inner?` field.
                                let inst = ir.add_expr(IrExpr::StaticInstance {
                                    owner: nsid,
                                    ty: nsid,
                                    field: "INSTANCE",
                                });
                                let method = if is_nullable(ty) {
                                    "encodeNullableSerializableElement"
                                } else {
                                    "encodeSerializableElement"
                                };
                                stmts.push(ir.add_expr(IrExpr::Call {
                                    callee: virtual_iface(
                                        "kotlinx/serialization/encoding/CompositeEncoder",
                                        method,
                                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                                    ),
                                    dispatch_receiver: Some(c),
                                    args: vec![d, idx, inst, v],
                                }));
                            } else if is_nullable(ty) {
                                // Nullable element: encodeNullableSerializableElement(desc, i,
                                // <Elem>Serializer.INSTANCE, value.getX()) — the encoder writes JSON
                                // null when the value is null. Only reference elements (no boxing) for
                                // now; nullable primitives bail to a clean no-op.
                                if let Some(ser) = builtin_element_serializer(ty) {
                                    let inst = ir.add_expr(IrExpr::ExternalStaticInstance {
                                        owner: ser.to_string(),
                                        ty: ser.to_string(),
                                        field: "INSTANCE".to_string(),
                                    });
                                    stmts.push(ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeEncoder",
                                            "encodeNullableSerializableElement",
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                                        ),
                                        dispatch_receiver: Some(c),
                                        args: vec![d, idx, inst, v],
                                    }));
                                } else {
                                    bail = true;
                                    break;
                                }
                            } else if let Some((mname, mdesc)) = encode_element_method(ty) {
                                stmts.push(ir.add_expr(IrExpr::Call {
                                    callee: virtual_iface(
                                        "kotlinx/serialization/encoding/CompositeEncoder",
                                        mname,
                                        mdesc,
                                    ),
                                    dispatch_receiver: Some(c),
                                    args: vec![d, idx, v],
                                }));
                            } else {
                                bail = true;
                                break;
                            }
                        }
                        if bail {
                            // a field type we can't encode yet — emit a clean no-op return (no
                            // beginStructure/endStructure), not a wrong call.
                            let ret = ir.add_expr(IrExpr::Return(None));
                            let body = ir.add_expr(IrExpr::Block {
                                stmts: vec![ret],
                                value: None,
                            });
                            ir.functions[fid as usize].body = Some(body);
                        } else {
                            let dend = this_desc(ir);
                            let cend = ir.add_expr(IrExpr::GetValue(3));
                            stmts.push(ir.add_expr(IrExpr::Call {
                                callee: virtual_iface(
                                    "kotlinx/serialization/encoding/CompositeEncoder",
                                    "endStructure",
                                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
                                ),
                                dispatch_receiver: Some(cend),
                                args: vec![dend],
                            }));
                            let body = ir.add_expr(IrExpr::Block { stmts, value: None });
                            ir.functions[fid as usize].body = Some(body);
                        }
                    }
                    "deserialize" => {
                        // deserialize(decoder=1):
                        //   val c = decoder.beginStructure(descriptor)            [local 2]
                        //   var f0 = <default>; var f1 = <default>                 [locals 4, 5, ...]
                        //   loop@ while (true) {
                        //       val i = c.decodeElementIndex(descriptor)           [local 3]
                        //       when (i) { -1 -> break@loop; 0 -> f0 = c.decode<T>Element(d,0); ... }
                        //   }
                        //   c.endStructure(descriptor); return Foo(f0, f1)
                        // Supported only when every field is a decodable type; otherwise fall back to
                        // a default-construct stub (no wrong decode emitted).
                        let ser_cid = ser_idx as u32;
                        let decodable = fields.iter().enumerate().all(|(i, (_, t))| {
                            if nested[i].is_some() {
                                return true;
                            }
                            if is_nullable(t) {
                                return builtin_element_serializer(t).is_some();
                            }
                            decode_element_method(t).is_some()
                        });
                        // Field-local slot for each property — `this`=0, decoder=1, c=2, i=3, then the
                        // field locals from slot 4, advancing by each type's JVM width (Long/Double=2).
                        let mut slots: Vec<u32> = Vec::with_capacity(fields.len());
                        let mut next = 4u32;
                        for (_, ty) in &fields {
                            slots.push(next);
                            next += slot_width(ty);
                        }
                        let this_desc = |ir: &mut IrFile| -> ExprId {
                            let r = ir.add_expr(IrExpr::GetValue(0));
                            ir.add_expr(IrExpr::GetField {
                                receiver: r,
                                class: ser_cid,
                                index: 0,
                            })
                        };
                        let body = if decodable {
                            let d0 = this_desc(ir);
                            let dec = ir.add_expr(IrExpr::GetValue(1));
                            let begin = ir.add_expr(IrExpr::Call {
                                callee: virtual_iface(
                                    "kotlinx/serialization/encoding/Decoder",
                                    "beginStructure",
                                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/CompositeDecoder;",
                                ),
                                dispatch_receiver: Some(dec),
                                args: vec![d0],
                            });
                            let cvar = ir.add_expr(IrExpr::Variable {
                                index: 2,
                                ty: class_ty("kotlinx/serialization/encoding/CompositeDecoder"),
                                init: Some(begin),
                            });
                            let mut stmts = vec![cvar];
                            // index local (3) — declared before the field locals so emit's slot order
                            // (by declaration) matches the explicit indices (this=0, decoder=1, c=2,
                            // i=3, f0=4, f1=5, …).
                            let izero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                            stmts.push(ir.add_expr(IrExpr::Variable {
                                index: 3,
                                ty: class_ty("kotlin/Int"),
                                init: Some(izero),
                            }));
                            // field locals (4 + k), defaulted (nullable refs start as null)
                            for (k, (_, ty)) in fields.iter().enumerate() {
                                let dc = if is_nullable(ty) {
                                    IrConst::Null
                                } else {
                                    default_const(ty)
                                };
                                let init = ir.add_expr(IrExpr::Const(dc));
                                stmts.push(ir.add_expr(IrExpr::Variable {
                                    index: slots[k],
                                    ty: ty.clone(),
                                    init: Some(init),
                                }));
                            }
                            // loop body: i = c.decodeElementIndex(desc); when(i){…}
                            let didx = this_desc(ir);
                            let cdi = ir.add_expr(IrExpr::GetValue(2));
                            let dei = ir.add_expr(IrExpr::Call {
                                callee: virtual_iface(
                                    "kotlinx/serialization/encoding/CompositeDecoder",
                                    "decodeElementIndex",
                                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)I",
                                ),
                                dispatch_receiver: Some(cdi),
                                args: vec![didx],
                            });
                            let set_i = ir.add_expr(IrExpr::SetValue { var: 3, value: dei });
                            // A sequence of single-branch `if` statements (each a Unit statement — no
                            // value mixing): `if (i == -1) break`, then `if (i == k) f_k = decode…`.
                            let mut loop_stmts = vec![set_i];
                            let iref = ir.add_expr(IrExpr::GetValue(3));
                            let neg1 = ir.add_expr(IrExpr::Const(IrConst::Int(-1)));
                            let is_done = ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: crate::ir::IrBinOp::Eq,
                                lhs: iref,
                                rhs: neg1,
                            });
                            let brk = ir.add_expr(IrExpr::Break {
                                label: Some("deser".to_string()),
                            });
                            let brk_blk = ir.add_expr(IrExpr::Block {
                                stmts: vec![brk],
                                value: None,
                            });
                            loop_stmts.push(ir.add_expr(IrExpr::When {
                                branches: vec![(Some(is_done), brk_blk)],
                            }));
                            for (k, (_, ty)) in fields.iter().enumerate() {
                                let iref = ir.add_expr(IrExpr::GetValue(3));
                                let kc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
                                let is_k = ir.add_expr(IrExpr::PrimitiveBinOp {
                                    op: crate::ir::IrBinOp::Eq,
                                    lhs: iref,
                                    rhs: kc,
                                });
                                let dk = this_desc(ir);
                                let idxc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
                                let cdk = ir.add_expr(IrExpr::GetValue(2));
                                let decoded = if let Some(nsid) = nested[k] {
                                    // f_k = (T) c.decode[Nullable]SerializableElement(desc, k,
                                    // T$serializer.INSTANCE, null). Same descriptor; the nullable
                                    // variant yields null for a JSON-null `Inner?` element.
                                    let inst = ir.add_expr(IrExpr::StaticInstance {
                                        owner: nsid,
                                        ty: nsid,
                                        field: "INSTANCE",
                                    });
                                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                                    let method = if is_nullable(ty) {
                                        "decodeNullableSerializableElement"
                                    } else {
                                        "decodeSerializableElement"
                                    };
                                    let raw = ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeDecoder",
                                            method,
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                                        ),
                                        dispatch_receiver: Some(cdk),
                                        args: vec![dk, idxc, inst, prev],
                                    });
                                    ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::Cast,
                                        arg: raw,
                                        type_operand: ty.clone(),
                                    })
                                } else if is_nullable(ty) {
                                    // f_k = (T) c.decodeNullableSerializableElement(desc, k,
                                    // <Elem>Serializer.INSTANCE, null) — yields the element or null.
                                    let ser = builtin_element_serializer(ty).unwrap();
                                    let inst = ir.add_expr(IrExpr::ExternalStaticInstance {
                                        owner: ser.to_string(),
                                        ty: ser.to_string(),
                                        field: "INSTANCE".to_string(),
                                    });
                                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                                    let raw = ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeDecoder",
                                            "decodeNullableSerializableElement",
                                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                                        ),
                                        dispatch_receiver: Some(cdk),
                                        args: vec![dk, idxc, inst, prev],
                                    });
                                    ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::Cast,
                                        arg: raw,
                                        type_operand: ty.clone(),
                                    })
                                } else {
                                    let (mname, mdesc) = decode_element_method(ty).unwrap();
                                    ir.add_expr(IrExpr::Call {
                                        callee: virtual_iface(
                                            "kotlinx/serialization/encoding/CompositeDecoder",
                                            mname,
                                            mdesc,
                                        ),
                                        dispatch_receiver: Some(cdk),
                                        args: vec![dk, idxc],
                                    })
                                };
                                let setk = ir.add_expr(IrExpr::SetValue {
                                    var: slots[k],
                                    value: decoded,
                                });
                                let setk_blk = ir.add_expr(IrExpr::Block {
                                    stmts: vec![setk],
                                    value: None,
                                });
                                loop_stmts.push(ir.add_expr(IrExpr::When {
                                    branches: vec![(Some(is_k), setk_blk)],
                                }));
                            }
                            let loop_body = ir.add_expr(IrExpr::Block {
                                stmts: loop_stmts,
                                value: None,
                            });
                            let cond = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
                            let whilexpr = ir.add_expr(IrExpr::While {
                                cond,
                                body: loop_body,
                                update: None,
                                post_test: false,
                                label: Some("deser".to_string()),
                            });
                            stmts.push(whilexpr);
                            // endStructure
                            let dend = this_desc(ir);
                            let cend = ir.add_expr(IrExpr::GetValue(2));
                            stmts.push(ir.add_expr(IrExpr::Call {
                                callee: virtual_iface(
                                    "kotlinx/serialization/encoding/CompositeDecoder",
                                    "endStructure",
                                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
                                ),
                                dispatch_receiver: Some(cend),
                                args: vec![dend],
                            }));
                            // return Foo(f0, f1, ...)
                            let args: Vec<ExprId> = (0..fields.len())
                                .map(|k| ir.add_expr(IrExpr::GetValue(slots[k])))
                                .collect();
                            let new = ir.add_expr(IrExpr::New {
                                class: foo_id,
                                args,
                                ctor_params: None,
                            });
                            stmts.push(ir.add_expr(IrExpr::Return(Some(new))));
                            ir.add_expr(IrExpr::Block { stmts, value: None })
                        } else {
                            // fallback: default-construct stub (no decode emitted for unsupported shapes).
                            // Nullable fields default to null, matching the real decode path (and so a
                            // future non-null default in `default_const` can't leak into a nullable slot).
                            let args: Vec<ExprId> = field_types
                                .iter()
                                .map(|ty| {
                                    let dc = if is_nullable(ty) {
                                        IrConst::Null
                                    } else {
                                        default_const(ty)
                                    };
                                    ir.add_expr(IrExpr::Const(dc))
                                })
                                .collect();
                            let new = ir.add_expr(IrExpr::New {
                                class: foo_id,
                                args,
                                ctor_params: None,
                            });
                            let ret = ir.add_expr(IrExpr::Return(Some(new)));
                            ir.add_expr(IrExpr::Block {
                                stmts: vec![ret],
                                value: None,
                            })
                        };
                        ir.functions[fid as usize].body = Some(body);
                    }
                    "childSerializers" => {
                        // Return the per-field element-serializer array (arity == field count): one
                        // `KSerializer` singleton per property. A nested `@Serializable` field uses the
                        // krusty-generated `<T>$serializer.INSTANCE`; a directly-supported field uses the
                        // builtin `…Serializer.INSTANCE`. An unsupported field type contributes `null`
                        // (placeholder) so the array arity still matches the descriptor's element count.
                        let elements: Vec<ExprId> = field_types
                            .iter()
                            .enumerate()
                            .map(|(i, ty)| {
                                if let Some(nsid) = nested[i] {
                                    ir.add_expr(IrExpr::StaticInstance {
                                        owner: nsid,
                                        ty: nsid,
                                        field: "INSTANCE",
                                    })
                                } else if let Some(ser) = builtin_element_serializer(ty) {
                                    ir.add_expr(IrExpr::ExternalStaticInstance {
                                        owner: ser.to_string(),
                                        ty: ser.to_string(),
                                        field: "INSTANCE".to_string(),
                                    })
                                } else {
                                    ir.add_expr(IrExpr::Const(IrConst::Null))
                                }
                            })
                            .collect();
                        let arr = ir.add_expr(IrExpr::Vararg {
                            element_type: class_ty(KSERIALIZER_FQ),
                            elements,
                        });
                        let ret = ir.add_expr(IrExpr::Return(Some(arr)));
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![ret],
                            value: None,
                        });
                        ir.functions[fid as usize].body = Some(body);
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{synthetic_class, PluginHost};

    /// Build `@Serializable class <name>(<one val per field type>)` as IR + an annotation table.
    fn serializable_class(name: &str, field_types: &[&str]) -> (IrFile, PluginContext, u32) {
        let mut ir = IrFile::default();
        let mut c = synthetic_class(name);
        c.fields = field_types
            .iter()
            .enumerate()
            .map(|(i, ty)| (format!("f{i}"), class_ty(ty)))
            .collect();
        c.ctor_param_count = field_types.len() as u32;
        let id = ir.add_class(c);
        let mut ctx = PluginContext::default();
        ctx.class_annotations
            .insert(id, vec![SERIALIZABLE_FQ.to_string()]);
        (ir, ctx, id)
    }

    fn run(ir: &mut IrFile, ctx: &PluginContext) {
        let mut host = PluginHost::new();
        host.register(Box::new(SerializationPlugin::default()));
        host.run(ir, ctx);
    }

    fn find_class<'a>(ir: &'a IrFile, fq: &str) -> &'a crate::ir::IrClass {
        ir.classes
            .iter()
            .find(|c| c.fq_name == fq)
            .unwrap_or_else(|| panic!("class {fq} not found"))
    }

    fn calls_method(ir: &IrFile, name: &str) -> bool {
        ir.exprs.iter().any(|e| {
            matches!(e, IrExpr::Call { callee: Callee::Virtual { name: n, .. }, .. } if n == name)
        })
    }

    fn refs_external_static(ir: &IrFile, owner: &str) -> bool {
        ir.exprs
            .iter()
            .any(|e| matches!(e, IrExpr::ExternalStaticInstance { owner: o, .. } if o == owner))
    }

    #[test]
    fn nullable_string_field_routes_through_nullable_serializable_calls() {
        // `@Serializable class N(val a: Int, val b: String?)` — the nullable element must go through
        // encode/decodeNullableSerializableElement with the builtin StringSerializer singleton, not
        // the plain encode/decodeStringElement path (which can't represent null).
        let (mut ir, ctx, id) = serializable_class("N", &["kotlin/Int", "kotlin/String"]);
        if let IrType::Class { nullable, .. } = &mut ir.classes[id as usize].fields[1].1 {
            *nullable = true;
        }
        run(&mut ir, &ctx);
        assert!(
            calls_method(&ir, "encodeNullableSerializableElement"),
            "serialize must use encodeNullableSerializableElement for String?"
        );
        assert!(
            calls_method(&ir, "decodeNullableSerializableElement"),
            "deserialize must use decodeNullableSerializableElement for String?"
        );
        assert!(
            refs_external_static(&ir, "kotlinx/serialization/internal/StringSerializer"),
            "must getstatic StringSerializer.INSTANCE as the element serializer"
        );
        // The non-null Int element still uses the direct primitive path.
        assert!(
            calls_method(&ir, "encodeIntElement") && calls_method(&ir, "decodeIntElement"),
            "non-null Int still uses encode/decodeIntElement"
        );
    }

    #[test]
    fn nullable_primitive_uses_boxed_name_and_builtin_serializer() {
        // krusty lowers a nullable primitive to its BOXED fq name (`Int?` → `java/lang/Integer`,
        // nullable). The plugin must still route it through encodeNullableSerializableElement with the
        // builtin IntSerializer singleton — this guards the boxed-name mapping.
        let (mut ir, ctx, id) = serializable_class("P", &["kotlin/Int"]);
        ir.classes[id as usize].fields[0].1 = IrType::Class {
            fq_name: "java/lang/Integer".to_string(),
            type_args: vec![],
            nullable: true,
        };
        run(&mut ir, &ctx);
        assert!(
            calls_method(&ir, "encodeNullableSerializableElement")
                && calls_method(&ir, "decodeNullableSerializableElement"),
            "nullable Int? must use the nullable serializable calls"
        );
        assert!(
            refs_external_static(&ir, "kotlinx/serialization/internal/IntSerializer"),
            "nullable Int? must reference IntSerializer.INSTANCE"
        );
    }

    #[test]
    fn nullable_nested_composite_uses_nullable_serializable_calls() {
        // `@Serializable class Outer(val inner: Inner?, val label: String)` where Inner is itself
        // @Serializable — the nullable nested element must go through encode/decodeNullable-
        // SerializableElement (not the plain serializable element, which can't represent null).
        let mut ir = IrFile::default();
        let mut inner = synthetic_class("Inner");
        inner.fields = vec![("v".to_string(), class_ty("kotlin/Int"))];
        inner.ctor_param_count = 1;
        let inner_id = ir.add_class(inner);
        let mut outer = synthetic_class("Outer");
        outer.fields = vec![
            (
                "inner".to_string(),
                IrType::Class {
                    fq_name: "Inner".to_string(),
                    type_args: vec![],
                    nullable: true,
                },
            ),
            ("label".to_string(), class_ty("kotlin/String")),
        ];
        outer.ctor_param_count = 2;
        let outer_id = ir.add_class(outer);
        let mut ctx = PluginContext::default();
        ctx.class_annotations
            .insert(inner_id, vec![SERIALIZABLE_FQ.to_string()]);
        ctx.class_annotations
            .insert(outer_id, vec![SERIALIZABLE_FQ.to_string()]);
        run(&mut ir, &ctx);
        assert!(
            calls_method(&ir, "encodeNullableSerializableElement")
                && calls_method(&ir, "decodeNullableSerializableElement"),
            "Inner? must use the nullable serializable element calls"
        );
        // The non-null String field still uses the direct primitive path.
        assert!(
            calls_method(&ir, "encodeStringElement"),
            "non-null String still uses encodeStringElement"
        );
    }

    #[test]
    fn synthesizes_serializer_object_with_members() {
        let (mut ir, ctx, _) = serializable_class("demo/Foo", &["kotlin/Int", "kotlin/String"]);
        run(&mut ir, &ctx);

        let ser = find_class(&ir, "demo/Foo$serializer");
        assert!(ser.is_object, "$serializer is a singleton object");
        assert_eq!(ser.interfaces, vec![KSERIALIZER_FQ.to_string()]);
        // supertype is KSerializer<Foo> (parameterized by the serialized type).
        match &ser.supertypes[0] {
            IrType::Class {
                fq_name, type_args, ..
            } => {
                assert_eq!(fq_name, KSERIALIZER_FQ);
                assert_eq!(type_args, &vec![class_ty("demo/Foo")]);
            }
            other => panic!("expected KSerializer<Foo>, got {other:?}"),
        }
        let names: Vec<&str> = ser
            .methods
            .iter()
            .map(|&fid| ir.functions[fid as usize].name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "getDescriptor",
                "serialize",
                "deserialize",
                "childSerializers"
            ]
        );
    }

    #[test]
    fn serializer_accessor_reads_singleton() {
        let (mut ir, ctx, _) = serializable_class("demo/Foo", &["kotlin/Int"]);
        run(&mut ir, &ctx);

        // The class gains a static `serializer()` returning KSerializer<Foo>.
        let acc_fid = *find_class(&ir, "demo/Foo")
            .methods
            .iter()
            .find(|&&fid| ir.functions[fid as usize].name == "serializer")
            .expect("serializer() accessor synthesized");
        let acc = &ir.functions[acc_fid as usize];
        assert!(acc.is_static);
        match &acc.ret {
            IrType::Class { fq_name, .. } => assert_eq!(fq_name, KSERIALIZER_FQ),
            other => panic!("expected KSerializer, got {other:?}"),
        }
        // Its body returns the $serializer singleton (`return Foo$serializer.INSTANCE`).
        let ser_id = ir
            .classes
            .iter()
            .position(|c| c.fq_name == "demo/Foo$serializer")
            .unwrap() as u32;
        let Some(IrExpr::Block { stmts, .. }) = acc.body.map(|b| ir.expr(b).clone()) else {
            panic!("accessor body is not a block");
        };
        let Some(IrExpr::Return(Some(v))) = stmts.first().map(|&s| ir.expr(s).clone()) else {
            panic!("accessor body does not return a value");
        };
        match ir.expr(v) {
            IrExpr::StaticInstance { ty, field, .. } => {
                assert_eq!(*ty, ser_id);
                assert_eq!(*field, "INSTANCE");
            }
            other => panic!("expected StaticInstance, got {other:?}"),
        }
    }

    /// The number of `addElement` calls in the `$serializer`'s `<init>` (its descriptor's element
    /// count) — the real, runtime-meaningful "arity tracks the field list" (verified end-to-end by
    /// the encode round-trip in `serialization_roundtrip_e2e`).
    fn descriptor_add_element_count(ir: &IrFile, ser_fq: &str) -> usize {
        let ser = find_class(ir, ser_fq);
        let Some(init) = ser.init_body else {
            return 0;
        };
        let IrExpr::Block { stmts, .. } = ir.expr(init) else {
            panic!("init_body is not a block");
        };
        stmts
            .iter()
            .filter(|&&s| {
                matches!(ir.expr(s), IrExpr::Call { callee: Callee::Virtual { name, .. }, .. } if name == "addElement")
            })
            .count()
    }

    #[test]
    fn descriptor_element_count_tracks_field_count() {
        for fields in [
            &[][..],
            &["kotlin/Int"][..],
            &["kotlin/Int", "kotlin/String", "kotlin/Boolean"][..],
        ] {
            let (mut ir, ctx, _) = serializable_class("demo/C", fields);
            run(&mut ir, &ctx);
            assert_eq!(
                descriptor_add_element_count(&ir, "demo/C$serializer"),
                fields.len(),
                "one descriptor element per field"
            );
        }
    }

    #[test]
    fn body_phase_fills_serialize_deserialize() {
        let (mut ir, ctx, _) = serializable_class("demo/Foo", &["kotlin/Int"]);
        SerializationPlugin::default().generate_declarations(&mut ir, &ctx);
        let before = ir
            .functions
            .iter()
            .filter(|f| f.name == "serialize" || f.name == "deserialize")
            .all(|f| f.body.is_none());
        assert!(before, "decl phase leaves serialize/deserialize empty");

        SerializationPlugin::default().transform_bodies(&mut ir, &ctx);
        let after = ir
            .functions
            .iter()
            .filter(|f| f.name == "serialize" || f.name == "deserialize")
            .all(|f| f.body.is_some());
        assert!(after, "body phase fills serialize/deserialize");
    }

    #[test]
    fn multiple_serializable_classes_each_get_a_serializer() {
        let mut ir = IrFile::default();
        let mut ctx = PluginContext::default();
        for (name, n) in [("demo/A", 1usize), ("demo/B", 2)] {
            let mut c = synthetic_class(name);
            c.fields = (0..n)
                .map(|i| (format!("f{i}"), class_ty("kotlin/Int")))
                .collect();
            let id = ir.add_class(c);
            ctx.class_annotations
                .insert(id, vec![SERIALIZABLE_FQ.to_string()]);
        }
        run(&mut ir, &ctx);
        assert!(ir.classes.iter().any(|c| c.fq_name == "demo/A$serializer"));
        assert!(ir.classes.iter().any(|c| c.fq_name == "demo/B$serializer"));
    }

    #[test]
    fn write_self_name_follows_target_abi() {
        // The plugin generates per target runtime version: the write helper is unmangled on <1.6 and
        // module-mangled on >=1.6 — like krusty pinning a kotlinc version, codegen follows the target.
        let names = |abi| {
            let (mut ir, ctx, class_id) = serializable_class("demo/Foo", &["kotlin/Int"]);
            SerializationPlugin::new(abi, "app").generate_declarations(&mut ir, &ctx);
            ir.classes[class_id as usize]
                .methods
                .iter()
                .map(|&f| ir.functions[f as usize].name.clone())
                .collect::<Vec<_>>()
        };
        assert!(names(SerializationAbi::V1_0).contains(&"write$Self".to_string()));
        assert!(names(SerializationAbi::V1_6Plus).contains(&"write$Self$app".to_string()));
        // ...and the two versions do NOT produce the same helper name.
        assert!(!names(SerializationAbi::V1_0).contains(&"write$Self$app".to_string()));
    }

    #[test]
    fn abi_detected_from_classpath_runtime() {
        // Drop-in: the ABI follows the kotlinx-serialization-core jar on -classpath, not a flag.
        assert_eq!(
            SerializationAbi::from_core_version("1.5.0"),
            SerializationAbi::V1_0
        );
        assert_eq!(
            SerializationAbi::from_core_version("1.6.0"),
            SerializationAbi::V1_6Plus
        );
        assert_eq!(
            SerializationAbi::from_core_version("1.8.1"),
            SerializationAbi::V1_6Plus
        );

        let cp = vec![
            "/x/kotlin-stdlib.jar".to_string(),
            "/x/kotlinx-serialization-core-jvm-1.8.1.jar".to_string(),
        ];
        assert_eq!(
            SerializationAbi::from_classpath(&cp),
            Some(SerializationAbi::V1_6Plus)
        );

        let old = vec!["/x/kotlinx-serialization-core-1.5.0.jar".to_string()];
        assert_eq!(
            SerializationAbi::from_classpath(&old),
            Some(SerializationAbi::V1_0)
        );

        // No serialization runtime on the classpath → no ABI (annotation wouldn't resolve either).
        assert_eq!(
            SerializationAbi::from_classpath(&["/x/kotlin-stdlib.jar".to_string()]),
            None
        );

        // -core, not -json/-protobuf, drives the ABI even when several serialization jars co-exist.
        let many = vec![
            "/x/kotlinx-serialization-json-jvm-1.5.0.jar".to_string(),
            "/x/kotlinx-serialization-protobuf-jvm-1.5.0.jar".to_string(),
            "/x/kotlinx-serialization-core-jvm-1.8.1.jar".to_string(),
        ];
        assert_eq!(
            SerializationAbi::from_classpath(&many),
            Some(SerializationAbi::V1_6Plus)
        );

        // A snapshot version still parses by its leading numeric components.
        let snap = vec!["/x/kotlinx-serialization-core-jvm-1.8.1-SNAPSHOT.jar".to_string()];
        assert_eq!(
            SerializationAbi::from_classpath(&snap),
            Some(SerializationAbi::V1_6Plus)
        );
    }

    #[test]
    fn non_serializable_class_untouched() {
        let mut ir = IrFile::default();
        ir.add_class(synthetic_class("demo/Plain"));
        let before = ir.classes.len();
        run(&mut ir, &PluginContext::default());
        assert_eq!(ir.classes.len(), before, "no @Serializable → no synthesis");
    }
}
