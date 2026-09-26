//! The class a property reference compiles to: its `PropertyReferenceNImpl` carrier, constructor,
//! `get`/`set` dispatch to the property's accessors and singleton instance.

use super::*;

fn emit_object_as(cw: &mut ClassWriter, code: &mut CodeBuilder, ty: Ty) {
    let ty = ir_ty_to_jvm(&ty);
    if ty.is_jvm_scalar() {
        // kotlinc coerces an erased `Object` to a number through `java/lang/Number`.
        unbox_prim_from(cw, code, Ty::obj("java/lang/Object"), ty);
    } else if let Some(internal) = checkcast_internal(ty) {
        let class = cw.class_ref(&internal);
        code.checkcast(class);
    }
}

fn property_getter_descriptor(pr: &crate::ir::PropRef, ext: bool) -> String {
    match (&pr.getter_descriptor, ext) {
        (Some(descriptor), _) => descriptor.clone(),
        (None, false) => format!("(){}", type_descriptor(ir_ty_to_jvm(&pr.prop_ty))),
        (None, true) => unreachable!(
            "a checked extension-property reference reached JVM emission without its getter descriptor"
        ),
    }
}

fn property_setter_target(pr: &crate::ir::PropRef, ext: bool) -> (String, String) {
    let name = pr
        .setter_name
        .clone()
        .unwrap_or_else(|| property_setter_name(&pr.prop_name));
    let descriptor = match (&pr.setter_descriptor, ext) {
        (Some(descriptor), _) => descriptor.clone(),
        (None, false) => format!("({})V", type_descriptor(ir_ty_to_jvm(&pr.prop_ty))),
        (None, true) => unreachable!(
            "a checked mutable extension-property reference reached JVM emission without its setter descriptor"
        ),
    };
    (name, descriptor)
}

struct PropertyCallTarget<'a> {
    owner: &'a str,
    facade: Option<&'a str>,
    array_length: bool,
    name: &'a str,
    descriptor: &'a str,
    params: &'a [Ty],
    owner_is_interface: bool,
    boxed_value_class: Option<TypeName>,
    /// The receiver is a value class's boxed object while the accessor takes its carrier.
    unboxed_receiver_value_class: Option<TypeName>,
    field_access: Option<&'a crate::jvm::property_references::PropertyFieldAccess>,
}

struct PropertyReferenceTarget {
    owner: String,
    call_owner: String,
    facade: Option<String>,
    array_length: bool,
    getter_descriptor: String,
    getter_params: Vec<Ty>,
    getter_ret: Ty,
    signature: String,
    getter_field: Option<crate::jvm::property_references::PropertyFieldAccess>,
    setter_field: Option<crate::jvm::property_references::PropertyFieldAccess>,
    boxed_value_class: Option<TypeName>,
    unboxed_receiver_value_class: Option<TypeName>,
}

impl PropertyReferenceTarget {
    fn new(
        property: &crate::ir::PropRef,
        realization: &crate::jvm::property_references::PropertyReferenceRealization,
        facade: &str,
    ) -> Self {
        let semantic_owner = property.owner().expect("property reference owner");
        let array_owner = crate::jvm::names::array_class_descriptor(&semantic_owner);
        let owner = array_owner.clone().unwrap_or_else(|| {
            crate::jvm::jvm_class_map::to_jvm_internal(&semantic_owner).to_string()
        });
        let semantic_call_owner = property
            .call_owner()
            .expect("property reference call owner");
        let call_owner = crate::jvm::names::array_class_descriptor(&semantic_call_owner)
            .unwrap_or_else(|| {
                crate::jvm::jvm_class_map::to_jvm_internal(&semantic_call_owner).to_string()
            });
        let facade = property.ext_facade_or_facade(facade);
        let getter_descriptor = property_getter_descriptor(property, facade.is_some());
        let (getter_params, getter_ret) = parse_physical_method_desc(&getter_descriptor)
            .expect("validated property getter descriptor");
        Self {
            owner,
            call_owner,
            array_length: array_owner.is_some() && facade.is_none(),
            signature: format!("{}{}", property.getter_name, getter_descriptor),
            facade,
            getter_descriptor,
            getter_params,
            getter_ret: ir_ty_to_jvm(&getter_ret),
            getter_field: realization.getter_field.clone(),
            setter_field: realization.setter_field.clone(),
            boxed_value_class: realization.boxed_value_class,
            unboxed_receiver_value_class: realization.unboxed_receiver_value_class,
        }
    }

    fn getter<'a>(&'a self, property: &'a crate::ir::PropRef) -> PropertyCallTarget<'a> {
        PropertyCallTarget {
            owner: &self.call_owner,
            facade: self.facade.as_deref(),
            array_length: self.array_length,
            name: &property.getter_name,
            descriptor: &self.getter_descriptor,
            params: &self.getter_params,
            owner_is_interface: property.owner_is_interface,
            boxed_value_class: self.boxed_value_class,
            unboxed_receiver_value_class: self.unboxed_receiver_value_class,
            field_access: self.getter_field.as_ref(),
        }
    }

    fn setter<'a>(
        &'a self,
        property: &'a crate::ir::PropRef,
        name: &'a str,
        descriptor: &'a str,
        params: &'a [Ty],
    ) -> PropertyCallTarget<'a> {
        PropertyCallTarget {
            owner: &self.call_owner,
            facade: self.facade.as_deref(),
            array_length: false,
            name,
            descriptor,
            params,
            owner_is_interface: property.owner_is_interface,
            boxed_value_class: self.boxed_value_class,
            unboxed_receiver_value_class: self.unboxed_receiver_value_class,
            field_access: self.setter_field.as_ref(),
        }
    }
}

fn emit_property_reference_constructor(
    cw: &mut ClassWriter,
    class: &str,
    superclass: &str,
    property: &crate::ir::PropRef,
    target: &PropertyReferenceTarget,
    bound: bool,
) {
    let own_descriptor = if bound {
        "(Ljava/lang/Object;)V"
    } else {
        "()V"
    };
    seed_method_header(cw, "<init>", own_descriptor);
    let mut code = CodeBuilder::new(if bound { 2 } else { 1 });
    code.aload(0);
    if bound {
        code.aload(1);
    }
    code.ldc_class(target.facade.as_deref().unwrap_or(&target.owner), cw);
    code.push_string(&property.prop_name, cw);
    code.push_string(&target.signature, cw);
    code.push_int(target.facade.is_some() as i32, cw);
    let descriptor = if bound {
        "(Ljava/lang/Object;Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
    } else {
        "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
    };
    let constructor = cw.methodref(superclass, "<init>", descriptor);
    code.invokespecial(constructor, if bound { 5 } else { 4 }, 0);
    code.ret_void();
    finish_code::<0x0000>(
        cw,
        "<init>",
        own_descriptor,
        &mut code,
        if bound { 2 } else { 1 },
    );
    let receiver: &[&str] = if bound { &["receiver"] } else { &[] };
    let locals = function_reference_invoke::reference_constructor_locals(cw, class, receiver);
    cw.set_method_debug("<init>", own_descriptor, None, &locals);
}

impl PropertyCallTarget<'_> {
    fn emit_get(&self, cw: &mut ClassWriter, code: &mut CodeBuilder, ret: Ty) {
        if let Some(access) = self.field_access {
            let physical = ir_ty_to_jvm(&access.ty);
            let owner = access.owner.render();
            let field = cw.fieldref(&owner, &access.name, &type_descriptor(physical));
            if access.is_static {
                code.pop();
                code.getstatic(field, slot_words(physical) as i32);
            } else {
                emit_object_as(cw, code, Ty::obj_name(access.owner));
                code.getfield(field, slot_words(physical) as i32);
            }
            return;
        }
        if self.array_length {
            let class = cw.class_ref(self.owner);
            code.checkcast(class);
            code.arraylength();
        } else if let Some(facade) = self.facade {
            adapt_property_reference_value(
                cw,
                code,
                self.unboxed_receiver_value_class,
                self.params[0],
            );
            let method = cw.methodref(facade, self.name, self.descriptor);
            code.invokestatic(
                method,
                slot_words(ir_ty_to_jvm(&self.params[0])) as i32,
                slot_words(ret) as i32,
            );
        } else if let Some(value_class) = self.unboxed_receiver_value_class {
            // A MEMBER of a value class: its accessor is realized as a static method over the
            // carrier on the value class itself, so the receiver is unboxed and the call is static.
            adapt_property_reference_value(cw, code, Some(value_class), self.params[0]);
            let method = cw.methodref(self.owner, self.name, self.descriptor);
            code.invokestatic(
                method,
                slot_words(ir_ty_to_jvm(&self.params[0])) as i32,
                slot_words(ret) as i32,
            );
        } else {
            emit_object_as(cw, code, Ty::obj(self.owner));
            if self.owner_is_interface {
                let method = cw.interface_methodref(self.owner, self.name, self.descriptor);
                code.invokeinterface(method, 0, slot_words(ret) as i32);
            } else {
                let method = cw.methodref(self.owner, self.name, self.descriptor);
                code.invokevirtual(method, 0, slot_words(ret) as i32);
            }
        }
    }

    fn emit_set(&self, cw: &mut ClassWriter, code: &mut CodeBuilder, value_local: u16) {
        if let Some(access) = self.field_access {
            let physical = ir_ty_to_jvm(&access.ty);
            if access.is_static {
                code.pop();
            } else {
                emit_object_as(cw, code, Ty::obj_name(access.owner));
            }
            code.aload(value_local);
            self.emit_property_value(cw, code, physical);
            let owner = access.owner.render();
            let field = cw.fieldref(&owner, &access.name, &type_descriptor(physical));
            if access.is_static {
                code.putstatic(field, slot_words(physical) as i32);
            } else {
                code.putfield(field, slot_words(physical) as i32);
            }
            return;
        }
        if let Some(facade) = self.facade {
            adapt_property_reference_value(
                cw,
                code,
                self.unboxed_receiver_value_class,
                self.params[0],
            );
            code.aload(value_local);
            self.emit_property_value(cw, code, self.params[1]);
            let arg_words = self
                .params
                .iter()
                .map(|param| slot_words(ir_ty_to_jvm(param)) as i32)
                .sum();
            let method = cw.methodref(facade, self.name, self.descriptor);
            code.invokestatic(method, arg_words, 0);
        } else if let Some(value_class) = self.unboxed_receiver_value_class {
            adapt_property_reference_value(cw, code, Some(value_class), self.params[0]);
            code.aload(value_local);
            self.emit_property_value(cw, code, self.params[1]);
            let arg_words = self
                .params
                .iter()
                .map(|param| slot_words(ir_ty_to_jvm(param)) as i32)
                .sum();
            let method = cw.methodref(self.owner, self.name, self.descriptor);
            code.invokestatic(method, arg_words, 0);
        } else {
            emit_object_as(cw, code, Ty::obj(self.owner));
            code.aload(value_local);
            self.emit_property_value(cw, code, self.params[0]);
            let value_words = slot_words(ir_ty_to_jvm(&self.params[0])) as i32;
            if self.owner_is_interface {
                let method = cw.interface_methodref(self.owner, self.name, self.descriptor);
                code.invokeinterface(method, value_words, 0);
            } else {
                let method = cw.methodref(self.owner, self.name, self.descriptor);
                code.invokevirtual(method, value_words, 0);
            }
        }
    }

    fn emit_property_value(&self, cw: &mut ClassWriter, code: &mut CodeBuilder, physical: Ty) {
        adapt_property_reference_value(cw, code, self.boxed_value_class, physical);
    }
}

/// Bring an erased `Object` on the stack to the accessor's PHYSICAL parameter type: a value class's
/// boxed object is cast and unboxed to its carrier, anything else is cast or unboxed as usual.
fn adapt_property_reference_value(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    boxed_value_class: Option<TypeName>,
    physical: Ty,
) {
    let Some(value_class) = boxed_value_class else {
        emit_object_as(cw, code, physical);
        return;
    };
    let owner = value_class.render();
    let class = cw.class_ref(&owner);
    code.checkcast(class);
    let descriptor = format!("(){}", type_descriptor(ir_ty_to_jvm(&physical)));
    let method = cw.methodref(&owner, "unbox-impl", &descriptor);
    code.invokevirtual(method, 0, slot_words(ir_ty_to_jvm(&physical)) as i32);
}

/// Emit a synthesized property-reference singleton (`Type$prop$N extends PropertyReference1Impl`):
/// a package-private `final` class with a `public static final INSTANCE`, a constructor
/// `super(owner.class, name, "getName()desc", 0)`, a `get(Object)Object` override that reads
/// `((Owner) it).getName()` (boxing a primitive), and a `<clinit>` that builds the singleton. `.name`
/// is inherited from `PropertyReference1Impl` (returns the constructor's name argument).
pub(super) fn emit_prop_ref_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let pr = c.prop_ref.as_ref().unwrap();
    let realization = env
        .property_reference_realizations
        .get(c.fq_name_id())
        .expect("a synthesized property reference must retain its JVM realization");
    if pr.static_dispatch {
        return emit_toplevel_prop_ref_class(ir, c, pr, realization, facade, env, opts);
    }
    if pr.bound {
        return emit_bound_prop_ref_class(ir, c, pr, facade, env, opts);
    }
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = property_reference_writer(ir, c, facade, env, opts);

    let target = PropertyReferenceTarget::new(pr, realization, facade);
    emit_property_reference_constructor(&mut cw, &fq, &superclass, pr, &target, false);

    seed_method_header(&mut cw, "get", "(Ljava/lang/Object;)Ljava/lang/Object;");
    let mut get = CodeBuilder::new(2);
    get.aload(1);
    target
        .getter(pr)
        .emit_get(&mut cw, &mut get, target.getter_ret);
    box_property_reference_value(
        &mut cw,
        &mut get,
        pr,
        realization.boxed_value_class,
        target.getter_ret,
    );
    get.areturn();
    finish_code::<0x0001>(
        &mut cw,
        "get",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        &mut get,
        2,
    );
    attach_accessor_debug(
        &mut cw,
        c,
        "get",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        &["receiver0"],
    );

    if pr.mutable {
        let (setter, setter_desc) = property_setter_target(pr, target.facade.is_some());
        let (setter_params, _) =
            parse_physical_method_desc(&setter_desc).expect("validated property setter descriptor");
        seed_method_header(&mut cw, "set", "(Ljava/lang/Object;Ljava/lang/Object;)V");
        let mut set = CodeBuilder::new(3);
        set.aload(1);
        target
            .setter(pr, &setter, &setter_desc, &setter_params)
            .emit_set(&mut cw, &mut set, 2);
        set.ret_void();
        finish_code::<0x0001>(
            &mut cw,
            "set",
            "(Ljava/lang/Object;Ljava/lang/Object;)V",
            &mut set,
            3,
        );
        attach_accessor_debug(
            &mut cw,
            c,
            "set",
            "(Ljava/lang/Object;Ljava/lang/Object;)V",
            &["receiver0", "value"],
        );
    }

    emit_singleton_instance_clinit(&mut cw, &fq);
    // kotlinc visits a carrier's fields after its methods.
    add_singleton_instance_field(&mut cw, &fq);
    finish_local_synthetic_class(cw)
}

/// Emit a bound property-reference (`obj::prop` → `PropertyReference0Impl` subclass): a constructor
/// `(Object receiver)` delegating to `super(receiver, owner.class, name, "getName()desc", 0)` (the base
/// stores the receiver), and a no-arg `get()` reading `((Owner) this.receiver).getName()`. Constructed
/// per use with the captured receiver — no `INSTANCE` singleton.
fn emit_bound_prop_ref_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    pr: &crate::ir::PropRef,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = property_reference_writer(ir, c, facade, env, opts);

    let realization = env
        .property_reference_realizations
        .get(c.fq_name_id())
        .expect("a synthesized property reference must retain its JVM realization");
    let target = PropertyReferenceTarget::new(pr, realization, facade);
    emit_property_reference_constructor(&mut cw, &fq, &superclass, pr, &target, true);

    // `get()Object`: for a member ref `((Owner) this.receiver).getName()`; for an extension ref
    // `Facade.getName((Owner) this.receiver)`. Boxed if primitive.
    seed_method_header(&mut cw, "get", "()Ljava/lang/Object;");
    let mut get = CodeBuilder::new(1);
    get.aload(0);
    // kotlinc names the inherited field through the carrier itself.
    let recv_f = cw.fieldref(&fq, "receiver", "Ljava/lang/Object;");
    get.getfield(recv_f, 1);
    target
        .getter(pr)
        .emit_get(&mut cw, &mut get, target.getter_ret);
    box_property_reference_value(
        &mut cw,
        &mut get,
        pr,
        realization.boxed_value_class,
        target.getter_ret,
    );
    get.areturn();
    finish_code::<0x0001>(&mut cw, "get", "()Ljava/lang/Object;", &mut get, 1);
    attach_accessor_debug(&mut cw, c, "get", "()Ljava/lang/Object;", &[]);

    // `set(Object)V` (a bound `var` reference): `((Owner) this.receiver).setName(v)` after
    // casting/unboxing the argument to the property type.
    if pr.mutable {
        let (setter, setter_desc) = property_setter_target(pr, target.facade.is_some());
        let (setter_params, _) =
            parse_physical_method_desc(&setter_desc).expect("validated property setter descriptor");
        seed_method_header(&mut cw, "set", "(Ljava/lang/Object;)V");
        let mut set = CodeBuilder::new(2);
        set.aload(0);
        let recv_f = cw.fieldref(&fq, "receiver", "Ljava/lang/Object;");
        set.getfield(recv_f, 1);
        target
            .setter(pr, &setter, &setter_desc, &setter_params)
            .emit_set(&mut cw, &mut set, 1);
        set.ret_void();
        finish_code::<0x0001>(&mut cw, "set", "(Ljava/lang/Object;)V", &mut set, 2);
        attach_accessor_debug(&mut cw, c, "set", "(Ljava/lang/Object;)V", &["value"]);
    }
    finish_local_synthetic_class(cw)
}

/// Emit a top-level property reference (`::foo` → `(Mutable)PropertyReference0Impl` subclass): an
/// `INSTANCE` singleton whose `get()` does `invokestatic <facade>.getFoo()` (no receiver), and — for a
/// `var` — a `set(Object)` doing `invokestatic <facade>.setFoo(v)`. The super ctor is the 4-arg
/// `(Class, String, String, int)` form with top-level flags = 1. `owner_internal = None` is the facade
/// sentinel (the declaring file class, unknown until emit).
fn emit_toplevel_prop_ref_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    pr: &crate::ir::PropRef,
    realization: &crate::jvm::property_references::PropertyReferenceRealization,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Vec<u8> {
    let owner = pr.owner_or_facade(facade);
    let call_owner = pr.call_owner().unwrap_or_else(|| facade.to_string());
    let fq = c.fq_name();
    let superclass = c.superclass();
    let mut cw = property_reference_writer(ir, c, facade, env, opts);

    let prop_jvm = ir_ty_to_jvm(&pr.prop_ty);
    let prop_desc = type_descriptor(prop_jvm);
    // A receiverless accessor's descriptor is its property's own type. The PropRef's recorded
    // descriptor is not it: for a companion-block or access-bridged property that descriptor names
    // the owner it is called with, which this reference does not pass.
    //
    // A VALUE-CLASS-typed one is the exception, and the only one: its accessors exchange the
    // class's CARRIER, which is what the reference realization recorded on this exact target. The
    // boxed convention this path used to keep named `getTopLevel()LZ;` where the declaration is
    // `getTopLevel()I`.
    let carrier = realization.boxed_value_class.is_some();
    let getter_desc = match (&pr.getter_descriptor, carrier) {
        (Some(descriptor), true) => descriptor.clone(),
        _ => format!("(){prop_desc}"),
    };
    // The accessor returns the carrier the realization recorded beside that descriptor.
    let getter_jvm = if carrier {
        ir_ty_to_jvm(
            &realization
                .physical_getter_ret
                .expect("a carrier-realized getter records its physical return"),
        )
    } else {
        prop_jvm
    };
    let signature = format!("{}{}", pr.getter_name, getter_desc); // e.g. "getFoo()LBox;"

    // `<init>()V`: super(owner.class, "name", "getName()desc", 1).
    seed_method_header(&mut cw, "<init>", "()V");
    let mut ctor = CodeBuilder::new(1);
    ctor.aload(0);
    ctor.ldc_class(&owner, &mut cw);
    ctor.push_string(&pr.prop_name, &mut cw);
    ctor.push_string(&signature, &mut cw);
    ctor.push_int(1, &mut cw);
    let sup = cw.methodref(
        &superclass,
        "<init>",
        "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V",
    );
    ctor.invokespecial(sup, 4, 0);
    ctor.ret_void();
    finish_code::<0x0000>(&mut cw, "<init>", "()V", &mut ctor, 1);
    let locals = function_reference_invoke::reference_constructor_locals(&mut cw, &fq, &[]);
    cw.set_method_debug("<init>", "()V", None, &locals);

    // `get()Object`: invokestatic <facade>.getName(), boxed if primitive.
    seed_method_header(&mut cw, "get", "()Ljava/lang/Object;");
    let mut get = CodeBuilder::new(1);
    let gref = cw.methodref(&call_owner, &pr.getter_name, &getter_desc);
    get.invokestatic(gref, 0, slot_words(getter_jvm) as i32);
    if carrier {
        box_property_reference_value(
            &mut cw,
            &mut get,
            pr,
            realization.boxed_value_class,
            getter_jvm,
        );
    } else if prop_jvm.is_jvm_scalar() {
        box_prim_free(
            &mut cw,
            &mut get,
            semantic_scalar_adapter(pr.prop_ty, prop_jvm),
        );
    }
    get.areturn();
    finish_code::<0x0001>(&mut cw, "get", "()Ljava/lang/Object;", &mut get, 1);
    attach_accessor_debug(&mut cw, c, "get", "()Ljava/lang/Object;", &[]);

    // `set(Object)V` (a `var`): invokestatic <facade>.setName(v) after casting/unboxing the argument.
    if pr.mutable {
        // The NAME is always the one the reference recorded, exactly as the getter's is: the
        // realization that chose it is the only thing that knows a `@JvmName`, an access bridge or
        // a value-class mangle. Only the DESCRIPTOR depends on whether the accessors exchange the
        // carrier. Pairing the two made a boxed-storage value-class property — `var x: Z?`, whose
        // setter still mangles because its PARAMETER does — fall back to the plain spelling and
        // name a method the facade does not declare.
        let setter = pr
            .setter_name
            .clone()
            .unwrap_or_else(|| property_setter_name(&pr.prop_name));
        let setter_desc = match (&pr.setter_descriptor, carrier) {
            (Some(descriptor), true) => descriptor.clone(),
            _ => format!("({prop_desc})V"),
        };
        let setter_jvm = if carrier {
            ir_ty_to_jvm(
                &realization
                    .physical_setter_value
                    .expect("a carrier-realized setter records its physical value type"),
            )
        } else {
            prop_jvm
        };
        seed_method_header(&mut cw, "set", "(Ljava/lang/Object;)V");
        let mut set = CodeBuilder::new(2);
        set.aload(1);
        if carrier {
            // The argument arrives as the BOXED value class through the erased `set(Object)`; the
            // accessor takes the carrier.
            if let Some(value_class) = realization.boxed_value_class {
                let owner = value_class.render();
                let cref = cw.class_ref(&owner);
                set.checkcast(cref);
                let unbox = cw.methodref(
                    &owner,
                    "unbox-impl",
                    &format!("(){}", type_descriptor(setter_jvm)),
                );
                value_class_boundary_conversion(
                    &mut cw,
                    &mut set,
                    pr.prop_ty.is_nullable(),
                    |_, set| set.invokevirtual(unbox, 0, slot_words(setter_jvm) as i32),
                );
            }
        } else if prop_jvm.is_jvm_scalar() {
            let adapter = semantic_scalar_adapter(pr.prop_ty, prop_jvm);
            unbox_prim_from(&mut cw, &mut set, Ty::obj("java/lang/Object"), adapter);
        } else if let Some(internal) = checkcast_internal(prop_jvm) {
            let cref = cw.class_ref(&internal);
            set.checkcast(cref);
        }
        let sref = cw.methodref(&call_owner, &setter, &setter_desc);
        set.invokestatic(sref, slot_words(setter_jvm) as i32, 0);
        set.ret_void();
        finish_code::<0x0001>(&mut cw, "set", "(Ljava/lang/Object;)V", &mut set, 2);
        attach_accessor_debug(&mut cw, c, "set", "(Ljava/lang/Object;)V", &["value"]);
    }

    emit_singleton_instance_clinit(&mut cw, &fq);
    // kotlinc visits a carrier's fields after its methods.
    add_singleton_instance_field(&mut cw, &fq);
    finish_local_synthetic_class(cw)
}

/// kotlinc's header for a property-reference carrier: a synthetic final class, enclosed by the
/// scope the reference is written in, with an inner-only `InnerClasses` row for itself and each
/// class it names.
fn property_reference_writer(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> ClassWriter {
    let mut cw = new_writer(&c.fq_name(), &c.superclass(), opts);
    cw.set_access(0x1000 | 0x0010 | 0x0020); // SYNTHETIC | FINAL | SUPER
    if let Some((owner, method)) = class_enclosure(ir, c, facade) {
        match method {
            Some((name, descriptor)) => cw.set_enclosing_method(&owner, &name, &descriptor),
            None => cw.set_enclosing_class(&owner),
        }
    }
    env.inner_classes.register(&mut cw);
    cw
}

/// A `get`/`set` override's debug tables: its whole body is the reference's line, and its locals
/// are `this` followed by the erased parameters under kotlinc's generated names.
fn attach_accessor_debug(
    cw: &mut ClassWriter,
    c: &crate::ir::IrClass,
    name: &str,
    descriptor: &str,
    parameters: &[&str],
) {
    let locals =
        function_reference_invoke::reference_constructor_locals(cw, &c.fq_name(), parameters);
    let line = (c.decl_line != 0).then_some((0, c.decl_line));
    cw.set_method_debug(name, descriptor, line, &locals);
}

/// A method's name and descriptor intern before its code, as a writer visiting the method first
/// does.
fn seed_method_header(cw: &mut ClassWriter, name: &str, descriptor: &str) {
    cw.seed_utf8(name);
    cw.seed_utf8(descriptor);
}
