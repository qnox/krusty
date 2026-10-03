//! `$DefaultImpls` forwarders and the generic signature of a method moved onto that holder.
//!
//! The interface keeps the real body. The holder republishes it as a static method whose first
//! parameter is the interface receiver, and whose signature promotes the interface's type
//! parameters onto the method.

use super::method_defaults::{default_stub_access, default_stub_params};
use super::signature_formatter::{JvmSignatureFormatter, Wildcards};
use super::{emit_return, jvm_type_params, load, slot_words};
use crate::ir::IrFile;
use crate::jvm::classfile::{ClassWriter, CodeBuilder};
use crate::jvm::method_descriptors::jvm_declared_ty;
use crate::jvm::names::method_descriptor;
use crate::types::{Ty, TypeName};

pub(super) fn emit_default_stub_forward(ir: &IrFile, fid: u32, owner: &str, cw: &mut ClassWriter) {
    let f = &ir.functions[fid as usize];
    let ret = jvm_declared_ty(&f.ret);
    let stub_params = default_stub_params(ir, fid, Ty::obj(owner));
    let desc = method_descriptor(&stub_params, ret);
    let name = format!("{}$default", f.name);
    cw.reserve_method_name(&name);
    cw.reserve_descriptor(&desc);
    let argument_words = stub_params.iter().map(|t| slot_words(*t)).sum::<u16>();
    let mut code = CodeBuilder::new(argument_words);
    let mut slot = 0u16;
    for ty in &stub_params {
        load(*ty, slot, &mut code);
        slot += slot_words(*ty);
    }
    let target = cw.interface_methodref(owner, &name, &desc);
    code.invokestatic(target, argument_words as i32, slot_words(ret) as i32);
    emit_return(ret, &mut code);
    code.ensure_locals(argument_words);
    code.link();
    cw.add_method(default_stub_access(ir, fid), &name, &desc, &code);
    if let Some(&line) = ir.fn_decl_lines.get(&fid) {
        cw.set_method_lines(&name, &desc, &[(0, line)]);
    }
}

/// A moved interface body keeps the source method's generic signature, but `$DefaultImpls` makes
/// the interface receiver its first static parameter and promotes the interface's type parameters
/// to method type parameters. Transform the already-computed method signature so suspend and other
/// specialized signatures retain their exact tail rather than being reconstructed by ABI shape.
pub(super) fn holder_method_signature(
    formatter: &JvmSignatureFormatter<'_>,
    ir: &IrFile,
    receiver: TypeName,
    method_signature: Option<&str>,
    descriptor: &str,
) -> Option<String> {
    let class_signature = ir.class_signature_name(receiver);
    let class_type_params = class_signature
        .map(|signature| signature.type_params.as_slice())
        .unwrap_or_default();
    if class_type_params.is_empty() && method_signature.is_none() {
        return None;
    }

    fn leading_type_parameters(signature: &str) -> (&str, &str) {
        if !signature.starts_with('<') {
            return ("", signature);
        }
        let mut depth = 0usize;
        for (index, byte) in signature.bytes().enumerate() {
            match byte {
                b'<' => depth += 1,
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        return (&signature[1..index], &signature[index + 1..]);
                    }
                }
                _ => {}
            }
        }
        ("", signature)
    }

    let class_declaration = class_signature
        .and_then(|signature| jvm_type_params(formatter, signature))
        .unwrap_or_default();
    let class_declaration = class_declaration
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or_default();
    let base = method_signature.unwrap_or(descriptor);
    let (method_declaration, method_tail) = leading_type_parameters(base);
    let declaration = if class_declaration.is_empty() && method_declaration.is_empty() {
        String::new()
    } else {
        format!("<{class_declaration}{method_declaration}>")
    };

    let receiver_ty = if class_type_params.is_empty() {
        Ty::obj_name(receiver)
    } else {
        let arguments = class_type_params
            .iter()
            .map(|parameter| {
                let bound = parameter
                    .bounds
                    .first()
                    .map(|(bound, _)| *bound)
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                Ty::ty_param(&parameter.name, bound)
            })
            .collect::<Vec<_>>();
        Ty::obj_args_name(receiver, &arguments)
    };
    let receiver_signature = formatter.method_ty(&receiver_ty, Wildcards::Declared)?;
    let parameters = method_tail.strip_prefix('(')?;
    Some(format!("{declaration}({receiver_signature}{parameters}"))
}
