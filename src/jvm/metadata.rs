//! Minimal Kotlin `@Metadata` reader: decode the `d1` protobuf and report which functions are
//! `inline`, by their JVM `(name, descriptor)`. This is the complete inline-recognition the inliner
//! needs (the body `reifiedOperationMarker` scan only finds *reified* inline functions).
//!
//! Schema (kotlin `core/metadata/src/metadata.proto` + `metadata.jvm/.../jvm_metadata.proto`):
//!   Package.function = 3; Function.flags = 9 (IS_INLINE = bit 10); Function.name = 2;
//!   Function extension method_signature = 100 → JvmMethodSignature { name = 1, desc = 2 }.
//! String ids index the `d2` table.

use super::classreader::ClassInfo;
use std::collections::HashSet;

/// Decode the `@Metadata` `d1` string array to raw protobuf bytes. Modern metadata (since Kotlin 1.4)
/// stores each byte as one already-UTF8-decoded char.
fn decode_d1(d1: &[String]) -> Vec<u8> {
    d1.iter().flat_map(|s| s.chars().map(|c| c as u8)).collect()
}

/// A protobuf wire-format cursor over a message body.
struct Pb<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Pb<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self.b.get(self.i)?;
            self.i += 1;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn at_end(&self) -> bool {
        self.i >= self.b.len()
    }
    /// Skip a field's value given its wire type; `false` on a malformed/unsupported wire type.
    fn skip(&mut self, wire: u64) -> Option<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.bytes(8)?;
            }
            2 => {
                let n = self.varint()? as usize;
                self.bytes(n)?;
            }
            5 => {
                self.bytes(4)?;
            }
            _ => return None,
        }
        Some(())
    }
}

/// `IS_INLINE` is bit 10 of `Function.flags` (hasAnnotations·1 + Visibility·3 + Modality·2 +
/// MemberKind·2 + isOperator·1 + isInfix·1 → isInline).
const IS_INLINE_BIT: u64 = 1 << 10;

/// Parse a `JvmMethodSignature` (extension body) → `(name string id, desc string id)`.
fn parse_jvm_signature(body: &[u8]) -> Option<(u64, u64)> {
    let mut pb = Pb { b: body, i: 0 };
    let mut name = None;
    let mut desc = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => name = Some(pb.varint()?),
            (2, 0) => desc = Some(pb.varint()?),
            (_, w) => pb.skip(w)?,
        }
    }
    Some((name?, desc?))
}

/// Parse one `Function` message → `(is_inline, name string id, Option<(jvm name id, jvm desc id)>)`.
fn parse_function(body: &[u8]) -> Option<(bool, u64, Option<(u64, u64)>)> {
    let mut pb = Pb { b: body, i: 0 };
    let mut flags = 0u64;
    let mut name_id = 0u64;
    let mut sig = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (9, 0) => flags = pb.varint()?,             // flags
            (2, 0) => name_id = pb.varint()?,           // name (name id in table)
            (100, 2) => {                                // method_signature extension
                let n = pb.varint()? as usize;
                let ext = pb.bytes(n)?;
                sig = parse_jvm_signature(ext);
            }
            (_, w) => pb.skip(w)?,
        }
    }
    Some((flags & IS_INLINE_BIT != 0, name_id, sig))
}

/// All `inline` functions declared in a `Package` body: explicit JVM `(name, descriptor)` pairs (when
/// a `method_signature` extension is present) and the set of inline function *names* (always, from
/// `Function.name`) — the latter catches the common inline functions (`map`, `let`, …) whose JVM
/// signature equals the computed default, so they omit the extension.
fn package_inline(body: &[u8], d2: &[String]) -> (HashSet<(String, String)>, HashSet<String>) {
    let mut methods = HashSet::new();
    let mut names = HashSet::new();
    let mut pb = Pb { b: body, i: 0 };
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 2) => {
                // repeated Function function = 3
                let Some(len) = pb.varint() else { break };
                let Some(fbody) = pb.bytes(len as usize) else { break };
                if let Some((true, name_id, sig)) = parse_function(fbody) {
                    if let Some(n) = d2.get(name_id as usize) {
                        names.insert(n.clone());
                    }
                    if let Some((ni, di)) = sig {
                        if let (Some(n), Some(d)) = (d2.get(ni as usize), d2.get(di as usize)) {
                            methods.insert((n.clone(), d.clone()));
                        }
                    }
                }
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    (methods, names)
}

/// The JVM `(name, descriptor)` of every `inline` function in a class with an explicit method
/// signature in its `@Metadata`. (Common inline functions omit it — see [`inline_method_names`].)
pub fn inline_methods(ci: &ClassInfo) -> HashSet<(String, String)> {
    if ci.kotlin_d1.is_empty() {
        return HashSet::new();
    }
    package_inline(&decode_d1(&ci.kotlin_d1), &ci.kotlin_d2).0
}

/// The Kotlin names of every `inline` function in a class's `@Metadata`. A call to a method of one of
/// these names (in this class) is inline — descriptor-agnostic, so it catches the functions whose
/// signature equals the default and thus carry no explicit `method_signature`.
pub fn inline_method_names(ci: &ClassInfo) -> HashSet<String> {
    if ci.kotlin_d1.is_empty() {
        return HashSet::new();
    }
    package_inline(&decode_d1(&ci.kotlin_d1), &ci.kotlin_d2).1
}
