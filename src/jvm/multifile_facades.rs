//! `@file:JvmMultifileClass`: each source of the class compiles to a package-private part
//! (`Facade__FooKt`), and the module writes one public facade per class once every part is
//! emitted. The facade declares a static forwarder for each part member a caller may name, in
//! part order, and the parts' visible `const val` fields.
//!
//! kotlinc builds each forwarder from the part's own declaration, so the facade copies the part
//! method's flags, generic signature, annotations and parameter names rather than deciding them
//! again.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::class_node::{ClassMethod, ClassNode};
use crate::jvm::classfile::{
    ClassWriter, MetadataStamp, ACC_FINAL, ACC_PUBLIC, ACC_SUPER, ACC_SYNTHETIC,
};
use crate::jvm::declaration_collisions::{self, FacadeSlot};
use crate::jvm::ir_emit::KotlinMetadata;
use crate::jvm::method_node::{Insn, LocalVariable, MethodNode, Node};
use crate::types::type_name;

/// A part's visible `const val`, which only its facade declares.
#[derive(Clone, Debug)]
pub(crate) struct FacadeConstant {
    pub(crate) access: u16,
    pub(crate) name: String,
    pub(crate) desc: String,
    pub(crate) signature: Option<String>,
    pub(crate) value: Option<crate::ir::IrConst>,
    pub(crate) annotation: Option<String>,
}

/// What emitting a part leaves its facade: the part's declared methods, which the facade may
/// forward to, and the `const val` fields only the facade declares.
#[derive(Default)]
pub(crate) struct EmittedPart {
    members: RefCell<Vec<(String, String)>>,
    constants: RefCell<Vec<FacadeConstant>>,
}

impl EmittedPart {
    pub(crate) fn record_members(&self, members: Vec<(String, String)>) {
        *self.members.borrow_mut() = members;
    }

    pub(crate) fn record_constants(&self, constants: Vec<FacadeConstant>) {
        *self.constants.borrow_mut() = constants;
    }

    pub(crate) fn take(&self) -> (Vec<(String, String)>, Vec<FacadeConstant>) {
        (self.members.take(), self.constants.take())
    }
}

/// A part's class header. It is `final synthetic` and package-private, as only its facade is
/// public, and carries the file's annotations except the `@JvmName` that names the facade.
pub(crate) fn write_part_header(
    cw: &mut ClassWriter,
    ir: &crate::ir::IrFile,
    metadata: Option<&KotlinMetadata>,
    facade: &str,
) {
    cw.set_access(ACC_FINAL | ACC_SUPER | ACC_SYNTHETIC);
    let jvm_name = type_name("kotlin/jvm/JvmName");
    cw.set_class_annotations(&ir.file_annotations.without(jvm_name));
    if let Some(metadata) = metadata {
        cw.set_multifile_part_metadata(
            metadata.stamp,
            metadata.xi,
            (&metadata.d1, &metadata.d2),
            facade,
        );
    }
}

/// One emitted part.
struct Part {
    internal: String,
    class: Vec<u8>,
    /// The methods the part declares before its synthetic accessors, by name and descriptor.
    members: Vec<(String, String)>,
    constants: Vec<FacadeConstant>,
    /// The forwarded methods with the declarations they realize, checked across the facade.
    slots: Vec<FacadeSlot>,
}

/// The module's emitted parts, by the internal name of the facade each belongs to.
#[derive(Default)]
pub(crate) struct MultifileParts {
    facades: BTreeMap<String, Vec<Part>>,
}

impl MultifileParts {
    pub(crate) fn record(
        &mut self,
        facade: &str,
        internal: &str,
        class: &[u8],
        (members, constants): (Vec<(String, String)>, Vec<FacadeConstant>),
        slots: Vec<FacadeSlot>,
    ) {
        self.facades
            .entry(facade.to_string())
            .or_default()
            .push(Part {
                internal: internal.to_string(),
                class: class.to_vec(),
                members,
                constants,
                slots,
            });
    }

    /// The platform declaration clashes between the parts of each facade, which kotlinc reports
    /// for the module before writing any class.
    pub(crate) fn clashes(&self) -> Vec<String> {
        self.facades
            .values()
            .flat_map(|parts| {
                let parts = sorted_parts(parts);
                declaration_collisions::facade_clashes(
                    parts.iter().map(|part| part.slots.as_slice()),
                )
            })
            .collect()
    }

    /// Each facade's class file, by internal name.
    pub(crate) fn write(&self, stamp: MetadataStamp) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.facades
            .iter()
            .map(|(facade, parts)| {
                let bytes = write_facade(facade, parts, stamp)?;
                Ok((facade.clone(), bytes))
            })
            .collect()
    }
}

/// A facade's parts in the JVM's string order, as kotlinc sorts them.
fn sorted_parts(parts: &[Part]) -> Vec<&Part> {
    let mut parts = parts.iter().collect::<Vec<_>>();
    parts.sort_by(|left, right| {
        left.internal
            .encode_utf16()
            .cmp(right.internal.encode_utf16())
    });
    parts
}

fn write_facade(facade: &str, parts: &[Part], stamp: MetadataStamp) -> Result<Vec<u8>, String> {
    let parts = sorted_parts(parts);
    let nodes = parts
        .iter()
        .map(|part| {
            ClassNode::read(&part.class)
                .map_err(|error| format!("cannot read multifile part {}: {error:?}", part.internal))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut cw = ClassWriter::new(facade, "java/lang/Object");
    cw.set_access(ACC_PUBLIC | ACC_FINAL | ACC_SUPER);
    cw.set_major(
        nodes
            .iter()
            .map(|node| node.major)
            .max()
            .unwrap_or_default(),
    );
    cw.keep_visited_inner_classes();
    // A facade of one part is compiled from that part's file; one of several names none.
    if let [node] = &nodes[..] {
        cw.set_source_file(node.source_file.clone());
    }
    for (part, node) in parts.iter().zip(&nodes) {
        for method in &node.methods {
            let member = (method.name.clone(), method.desc.clone());
            if method.access & ACC_PUBLIC == 0 || !part.members.contains(&member) {
                continue;
            }
            let forwarder = forwarder(&node.name, method)?;
            cw.add_copied_method(&forwarder).map_err(|error| {
                format!(
                    "cannot forward {}.{}{} from its facade: {error:?}",
                    node.name, method.name, method.desc
                )
            })?;
        }
    }
    for constant in parts.iter().flat_map(|part| &part.constants) {
        cw.add_field_late_sig(
            constant.access,
            &constant.name,
            &constant.desc,
            constant.signature.as_deref(),
            constant.value.clone(),
            constant.annotation.as_deref(),
        );
    }
    let part_names = parts
        .iter()
        .map(|part| part.internal.clone())
        .collect::<Vec<_>>();
    cw.set_multifile_facade_metadata(stamp, 48, &part_names);
    Ok(cw.finish())
}

/// The facade's copy of `method`: the part method's declaration over a body that passes every
/// parameter to it. Its one line is the file's first, and its locals are the part's parameters.
fn forwarder(part: &str, method: &ClassMethod) -> Result<ClassMethod, String> {
    let malformed = || format!("malformed descriptor {}", method.desc);
    let arguments = descriptors::argument_types(&method.desc).ok_or_else(malformed)?;
    let result = descriptors::return_type(&method.desc).ok_or_else(malformed)?;
    let mut node = MethodNode::new(method.access, &method.name, &method.desc);
    let start = node.new_label();
    node.nodes.push(Node::Label(start));
    node.nodes.push(Node::Line { line: 1, start });
    let mut slot = 0u16;
    for argument in &arguments {
        node.nodes.push(Node::Insn(Insn::Var {
            op: descriptors::typed_opcode(argument, ILOAD),
            slot,
        }));
        slot += descriptors::size(argument) as u16;
    }
    node.nodes.push(Node::Insn(Insn::Method {
        op: INVOKESTATIC,
        owner: part.to_string(),
        name: method.name.clone(),
        desc: method.desc.clone(),
        interface: false,
    }));
    node.nodes.push(Node::Insn(Insn::Op(if result == "V" {
        RETURN
    } else {
        descriptors::typed_opcode(result, IRETURN)
    })));
    let end = node.new_label();
    node.nodes.push(Node::Label(end));
    let mut parameters: Vec<&LocalVariable> = method
        .code
        .iter()
        .flat_map(|code| &code.local_variables)
        .filter(|local| local.slot < slot)
        .collect();
    parameters.sort_by_key(|local| local.slot);
    parameters.dedup_by_key(|local| local.slot);
    node.local_variables = parameters
        .into_iter()
        .map(|local| LocalVariable {
            name: local.name.clone(),
            desc: local.desc.clone(),
            start,
            end,
            slot: local.slot,
        })
        .collect();
    Ok(ClassMethod {
        access: method.access,
        name: method.name.clone(),
        desc: method.desc.clone(),
        signature: method.signature.clone(),
        exceptions: method.exceptions.clone(),
        visible_annotations: method.visible_annotations.clone(),
        invisible_annotations: method.invisible_annotations.clone(),
        visible_parameter_annotations: method.visible_parameter_annotations.clone(),
        invisible_parameter_annotations: method.invisible_parameter_annotations.clone(),
        annotation_default: None,
        parameters: method.parameters.clone(),
        deprecated: method.deprecated,
        code: Some(node),
    })
}

const ILOAD: u8 = 0x15;
const IRETURN: u8 = 0xac;
const RETURN: u8 = 0xb1;
const INVOKESTATIC: u8 = 0xb8;

/// kotlinc names each private static method of a multifile part `<name>$<Part>`, so the parts of
/// one facade never collide: private functions, private accessors and lambda bodies alike. An
/// explicit `@JvmName` is the whole JVM name and keeps it.
pub(crate) fn suffix_private_part_members(
    ir: &mut crate::ir::IrFile,
    part: &str,
    continuations: &mut crate::jvm::suspend::ContinuationMetadataMap,
) {
    let simple_name = part.rsplit('/').next().unwrap_or(part);
    let class_members = ir
        .classes
        .iter()
        .flat_map(|class| class.methods.iter().copied())
        .collect::<crate::fx_hash::HashSet<_>>();
    let renamed = (0..ir.functions.len() as u32)
        .filter(|&function| {
            let declaration = &ir.functions[function as usize];
            declaration.dispatch_receiver.is_none()
                && declaration.name != "<clinit>"
                && !class_members.contains(&function)
                && !ir.class_static_local_functions.contains_key(&function)
                && ir.method_visibility(function).is_private()
                && matches!(
                    super::module_calls::applied_jvm_name(ir.function_annotations.get(&function)),
                    Ok(None)
                )
        })
        .collect::<Vec<_>>();
    for &function in &renamed {
        let declaration = &mut ir.functions[function as usize];
        let name = std::mem::take(&mut declaration.name);
        declaration.name = format!("{name}${simple_name}");
        ir.multifile_private_names.insert(function, name);
    }
    // A private suspend function's continuation is enclosed by the renamed method.
    for continuation in continuations.values_mut() {
        if let Some(function) = continuation
            .enclosing_function
            .filter(|function| renamed.contains(function))
        {
            continuation.enclosing_method = ir.functions[function as usize].name.clone();
        }
    }
    // A reference reflects the function by its JVM name, while its own name stays the Kotlin one.
    for class in &mut ir.classes {
        let Some(reference) = class.func_ref.as_mut() else {
            continue;
        };
        let target = match reference.reflected {
            crate::ir::ReflectedCallable::Source => reference.reflected_function,
            _ => None,
        };
        if let Some(target) = target.filter(|target| !reference.adapted && renamed.contains(target))
        {
            if reference.reflection_name.is_none() {
                reference.reflection_name = Some(ir.functions[target as usize].name.clone());
            }
        }
    }
}
