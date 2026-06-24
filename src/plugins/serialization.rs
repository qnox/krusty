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

use crate::ir::{Callee, ExprId, IrExpr, IrFile, IrFunction, IrType};
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

fn kserializer_of(arg: IrType) -> IrType {
    IrType::Class {
        fq_name: KSERIALIZER_FQ.to_string(),
        type_args: vec![arg],
        nullable: false,
    }
}

/// The element-serializer FqName for a field type — the runtime serializer kotlinc looks up for each
/// property. Built-ins map to `kotlinx.serialization.builtins.*Serializer`; a user `@Serializable`
/// type maps to its own `$serializer`. Makes `childSerializers` reflect each field's *type*, not just
/// the count.
fn element_serializer_fq(field_ty: &IrType) -> String {
    let fq = match field_ty {
        IrType::Class { fq_name, .. } => fq_name.as_str(),
        _ => "kotlin/Any",
    };
    match fq {
        "kotlin/Int" => "kotlinx/serialization/builtins/IntSerializer".to_string(),
        "kotlin/Long" => "kotlinx/serialization/builtins/LongSerializer".to_string(),
        "kotlin/Boolean" => "kotlinx/serialization/builtins/BooleanSerializer".to_string(),
        "kotlin/String" => "kotlinx/serialization/builtins/StringSerializer".to_string(),
        other => serializer_fq(other), // a nested @Serializable type uses its own $serializer
    }
}

impl SerializationPlugin {
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
        for class_id in ctx.classes_with(SERIALIZABLE_FQ) {
            let class_fq = ir.classes[class_id as usize].fq_name.clone();
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

            let mut ser = synthetic_class(&ser_fq);
            ser.is_object = true; // `$serializer` is a singleton object (INSTANCE)
            ser.interfaces = vec![KSERIALIZER_FQ.to_string()];
            ser.supertypes = vec![kserializer_of(class_ty(&class_fq))];
            ser.methods = vec![descriptor, serialize, deserialize, child];
            let ser_id = ir.add_class(ser);

            // `serializer()` accessor — body reads the `$serializer` singleton (`Foo$serializer.INSTANCE`).
            // kotlinc emits it on `Foo.Companion`; the PoC attaches it to the class as a static member.
            let inst = ir.add_expr(IrExpr::StaticInstance {
                owner: ser_id,
                ty: ser_id,
                field: "INSTANCE",
            });
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![],
                value: Some(inst),
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
            // on core >= 1.6). Emitted on the serialized class as a static member.
            let write_self = ir.add_fun(IrFunction {
                name: self.write_self_name(),
                params: vec![
                    class_ty(&class_fq),
                    class_ty("kotlinx/serialization/encoding/CompositeEncoder"),
                    class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                ],
                ret: unit(),
                body: None,
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
        for class_id in ctx.classes_with(SERIALIZABLE_FQ) {
            let field_types: Vec<IrType> = ir.classes[class_id as usize]
                .fields
                .iter()
                .map(|(_, ty)| ty.clone())
                .collect();
            let ser_fq = serializer_fq(&ir.classes[class_id as usize].fq_name);
            let Some(ser_idx) = ir.classes.iter().position(|c| c.fq_name == ser_fq) else {
                continue;
            };
            for fid in ir.classes[ser_idx].methods.clone() {
                match ir.functions[fid as usize].name.as_str() {
                    "serialize" | "deserialize" => {
                        let ret = ir.add_expr(IrExpr::Return(None));
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![ret],
                            value: None,
                        });
                        ir.functions[fid as usize].body = Some(body);
                    }
                    "childSerializers" => {
                        // One element serializer per property, in field order — the array's length is
                        // the field count, so the synthesized member is driven by the analyzed shape.
                        let elements: Vec<ExprId> = field_types
                            .iter()
                            .map(|ty| {
                                ir.add_expr(IrExpr::Call {
                                    callee: Callee::External(element_serializer_fq(ty)),
                                    dispatch_receiver: None,
                                    args: vec![],
                                })
                            })
                            .collect();
                        let arr = ir.add_expr(IrExpr::Vararg {
                            element_type: kserializer_of(class_ty("kotlin/Any")),
                            elements,
                        });
                        let body = ir.add_expr(IrExpr::Block {
                            stmts: vec![],
                            value: Some(arr),
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
        // Its body reads the $serializer singleton.
        let ser_id = ir
            .classes
            .iter()
            .position(|c| c.fq_name == "demo/Foo$serializer")
            .unwrap() as u32;
        let Some(IrExpr::Block { value: Some(v), .. }) = acc.body.map(|b| ir.expr(b).clone())
        else {
            panic!("accessor body is not a block");
        };
        match ir.expr(v) {
            IrExpr::StaticInstance { ty, field, .. } => {
                assert_eq!(*ty, ser_id);
                assert_eq!(*field, "INSTANCE");
            }
            other => panic!("expected StaticInstance, got {other:?}"),
        }
    }

    /// childSerializers' body is an array with exactly one element serializer per field, naming each
    /// field's serializer — so the synthesized member tracks BOTH count and per-field type.
    fn child_element_callees(ir: &IrFile, ser_fq: &str) -> Vec<String> {
        let ser = find_class(ir, ser_fq);
        let fid = *ser
            .methods
            .iter()
            .find(|&&f| ir.functions[f as usize].name == "childSerializers")
            .unwrap();
        let Some(IrExpr::Block { value: Some(v), .. }) =
            ir.functions[fid as usize].body.map(|b| ir.expr(b).clone())
        else {
            panic!("childSerializers body is not a block");
        };
        let IrExpr::Vararg { elements, .. } = ir.expr(v) else {
            panic!("childSerializers does not return an array");
        };
        elements
            .iter()
            .map(|&e| match ir.expr(e) {
                IrExpr::Call {
                    callee: Callee::External(fq),
                    ..
                } => fq.clone(),
                other => panic!("expected element serializer call, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn childserializers_tracks_field_count_and_types() {
        // 0 fields → empty array; 1; 3 → arity follows the field list.
        for fields in [
            &[][..],
            &["kotlin/Int"][..],
            &["kotlin/Int", "kotlin/String", "kotlin/Boolean"][..],
        ] {
            let (mut ir, ctx, _) = serializable_class("demo/C", fields);
            run(&mut ir, &ctx);
            let callees = child_element_callees(&ir, "demo/C$serializer");
            assert_eq!(callees.len(), fields.len(), "one serializer per field");
        }

        // Per-field type drives the element serializer name.
        let (mut ir, ctx, _) = serializable_class("demo/Foo", &["kotlin/Int", "kotlin/String"]);
        run(&mut ir, &ctx);
        assert_eq!(
            child_element_callees(&ir, "demo/Foo$serializer"),
            vec![
                "kotlinx/serialization/builtins/IntSerializer".to_string(),
                "kotlinx/serialization/builtins/StringSerializer".to_string(),
            ]
        );
    }

    #[test]
    fn nested_serializable_field_uses_own_serializer() {
        // A field of another @Serializable type resolves to that type's $serializer, not a builtin.
        let (mut ir, ctx, _) = serializable_class("demo/Outer", &["demo/Inner"]);
        run(&mut ir, &ctx);
        assert_eq!(
            child_element_callees(&ir, "demo/Outer$serializer"),
            vec!["demo/Inner$serializer".to_string()]
        );
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
