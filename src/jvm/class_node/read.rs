//! Reading a class file into a [`ClassNode`].

use std::sync::Arc;

use super::annotation::{Annotation, ElementValue};
use super::{ClassMethod, ClassNode, FieldNode, InnerClass, OuterClass};
use crate::jvm::classreader::{read_constant_pool, utf8_value, ClassBodies, C};
use crate::jvm::method_node::{Constant, MalformedCode, MethodNode};

/// Why a class could not be read as a whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClassReadError {
    /// The bytes are not a class file or end early.
    Malformed(&'static str),
    /// The class carries an attribute a copy would lose.
    UnsupportedAttribute(String),
    /// A method's `Code` could not be read.
    Code(String, MalformedCode),
}

use ClassReadError::Malformed;

const ACC_SYNTHETIC: u16 = 0x1000;

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn u1(&mut self) -> Result<u8, ClassReadError> {
        let byte = *self
            .bytes
            .get(self.at)
            .ok_or(Malformed("truncated class"))?;
        self.at += 1;
        Ok(byte)
    }

    fn u2(&mut self) -> Result<u16, ClassReadError> {
        Ok(u16::from(self.u1()?) << 8 | u16::from(self.u1()?))
    }

    fn u4(&mut self) -> Result<u32, ClassReadError> {
        Ok(u32::from(self.u2()?) << 16 | u32::from(self.u2()?))
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ClassReadError> {
        let end = self
            .at
            .checked_add(length)
            .ok_or(Malformed("truncated class"))?;
        let taken = self
            .bytes
            .get(self.at..end)
            .ok_or(Malformed("truncated class"))?;
        self.at = end;
        Ok(taken)
    }

    fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

struct Pool<'a>(&'a [C]);

impl Pool<'_> {
    fn utf8(&self, index: u16) -> Result<String, ClassReadError> {
        match self.0.get(usize::from(index)) {
            Some(C::Utf8(text)) => Ok(text.clone()),
            _ => Err(Malformed("a name is not a UTF-8 constant")),
        }
    }

    fn optional_utf8(&self, index: u16) -> Result<Option<String>, ClassReadError> {
        if index == 0 {
            Ok(None)
        } else {
            self.utf8(index).map(Some)
        }
    }

    fn class(&self, index: u16) -> Result<String, ClassReadError> {
        match self.0.get(usize::from(index)) {
            Some(C::Class(name)) => self.utf8(*name),
            _ => Err(Malformed("a class reference is not a class constant")),
        }
    }

    fn optional_class(&self, index: u16) -> Result<Option<String>, ClassReadError> {
        if index == 0 {
            Ok(None)
        } else {
            self.class(index).map(Some)
        }
    }

    fn name_and_type(&self, index: u16) -> Result<(String, String), ClassReadError> {
        match self.0.get(usize::from(index)) {
            Some(C::NameAndType(name, desc)) => Ok((self.utf8(*name)?, self.utf8(*desc)?)),
            _ => Err(Malformed(
                "an enclosing method is not a name-and-type constant",
            )),
        }
    }

    fn integer(&self, index: u16) -> Result<i32, ClassReadError> {
        match self.0.get(usize::from(index)) {
            Some(C::Integer(value)) => Ok(*value),
            _ => Err(Malformed("an integer constant was expected")),
        }
    }

    fn constant_value(&self, index: u16) -> Result<Constant, ClassReadError> {
        Ok(match self.0.get(usize::from(index)) {
            Some(C::Integer(value)) => Constant::Int(*value),
            Some(C::Float(bits)) => Constant::Float(*bits),
            Some(C::Long(value)) => Constant::Long(*value),
            Some(C::Double(bits)) => Constant::Double(*bits),
            Some(C::String(text)) => Constant::String(
                utf8_value(self.0, *text).ok_or(Malformed("a string constant has no text"))?,
            ),
            _ => return Err(Malformed("a field's constant value is not loadable")),
        })
    }
}

/// Read `bytes` as a class.
pub(super) fn read_class(bytes: &[u8]) -> Result<ClassNode, ClassReadError> {
    let (cp, after_pool) = read_constant_pool(bytes).map_err(|_| Malformed("unreadable pool"))?;
    let pool = Pool(&cp);
    let mut header = Cursor { bytes, at: 4 };
    // A copy is written under its caller's version, which has no minor version.
    if header.u2()? != 0 {
        return Err(ClassReadError::UnsupportedAttribute(
            "a minor version".to_string(),
        ));
    }
    let major = header.u2()?;
    let mut r = Cursor {
        bytes,
        at: after_pool,
    };
    let access = r.u2()?;
    let name = pool.class(r.u2()?)?;
    let super_name = pool.optional_class(r.u2()?)?;
    let interfaces = (0..r.u2()?)
        .map(|_| r.u2().and_then(|index| pool.class(index)))
        .collect::<Result<Vec<_>, _>>()?;

    let mut fields = Vec::new();
    for _ in 0..r.u2()? {
        fields.push(read_field(&mut r, &pool)?);
    }
    let mut declarations = Vec::new();
    for _ in 0..r.u2()? {
        declarations.push(read_method(&mut r, &pool)?);
    }

    let mut class = ClassNode {
        major,
        access,
        name,
        signature: None,
        super_name,
        interfaces,
        source_file: None,
        source_debug: None,
        outer_class: None,
        inner_classes: Vec::new(),
        visible_annotations: Vec::new(),
        invisible_annotations: Vec::new(),
        deprecated: false,
        fields,
        methods: Vec::new(),
    };
    for _ in 0..r.u2()? {
        let attribute = pool.utf8(r.u2()?)?;
        let length = r.u4()? as usize;
        let body = r.take(length)?;
        let mut a = Cursor { bytes: body, at: 0 };
        match attribute.as_str() {
            "Signature" => class.signature = Some(pool.utf8(a.u2()?)?),
            "SourceFile" => class.source_file = Some(pool.utf8(a.u2()?)?),
            "SourceDebugExtension" => {
                class.source_debug = Some(
                    String::from_utf8(body.to_vec())
                        .map_err(|_| Malformed("an SMAP is not UTF-8"))?,
                );
                a.at = body.len();
            }
            "EnclosingMethod" => {
                let owner = pool.class(a.u2()?)?;
                let method = match a.u2()? {
                    0 => None,
                    index => Some(pool.name_and_type(index)?),
                };
                class.outer_class = Some(OuterClass { owner, method });
            }
            "InnerClasses" => {
                for _ in 0..a.u2()? {
                    class.inner_classes.push(InnerClass {
                        name: pool.class(a.u2()?)?,
                        outer_name: pool.optional_class(a.u2()?)?,
                        inner_name: pool.optional_utf8(a.u2()?)?,
                        access: a.u2()?,
                    });
                }
            }
            "RuntimeVisibleAnnotations" => {
                class.visible_annotations = read_annotations(&mut a, &pool)?
            }
            "RuntimeInvisibleAnnotations" => {
                class.invisible_annotations = read_annotations(&mut a, &pool)?
            }
            "Deprecated" => class.deprecated = true,
            "Synthetic" => class.access |= ACC_SYNTHETIC,
            // The bodies read below resolve their `invokedynamic`s through it.
            "BootstrapMethods" => a.at = body.len(),
            _ => return Err(ClassReadError::UnsupportedAttribute(attribute)),
        }
        if !a.at_end() {
            return Err(Malformed("an attribute is longer than its contents"));
        }
    }
    if !r.at_end() {
        return Err(Malformed("bytes after the class"));
    }

    let bodies = ClassBodies::parse(Arc::new(bytes.to_vec()))
        .ok_or(Malformed("unreadable method bodies"))?;
    for declaration in declarations {
        let code = if declaration.has_code {
            let body = bodies
                .method_code(&declaration.method.name, &declaration.method.desc)
                .ok_or(Malformed("unreadable Code attribute"))?;
            let node = MethodNode::read(
                declaration.method.access,
                &declaration.method.name,
                &declaration.method.desc,
                &body,
            )
            .map_err(|error| ClassReadError::Code(declaration.method.name.clone(), error))?;
            Some(node)
        } else {
            None
        };
        class.methods.push(ClassMethod {
            code,
            ..declaration.method
        });
    }
    Ok(class)
}

fn read_field(r: &mut Cursor<'_>, pool: &Pool<'_>) -> Result<FieldNode, ClassReadError> {
    let mut field = FieldNode {
        access: r.u2()?,
        name: pool.utf8(r.u2()?)?,
        desc: pool.utf8(r.u2()?)?,
        signature: None,
        value: None,
        visible_annotations: Vec::new(),
        invisible_annotations: Vec::new(),
        deprecated: false,
    };
    for _ in 0..r.u2()? {
        let attribute = pool.utf8(r.u2()?)?;
        let length = r.u4()? as usize;
        let mut a = Cursor {
            bytes: r.take(length)?,
            at: 0,
        };
        match attribute.as_str() {
            "Signature" => field.signature = Some(pool.utf8(a.u2()?)?),
            "ConstantValue" => field.value = Some(pool.constant_value(a.u2()?)?),
            "RuntimeVisibleAnnotations" => {
                field.visible_annotations = read_annotations(&mut a, pool)?
            }
            "RuntimeInvisibleAnnotations" => {
                field.invisible_annotations = read_annotations(&mut a, pool)?
            }
            "Deprecated" => field.deprecated = true,
            // ASM reads the pre-1.5 attribute as the flag.
            "Synthetic" => field.access |= ACC_SYNTHETIC,
            _ => return Err(ClassReadError::UnsupportedAttribute(attribute)),
        }
        if !a.at_end() {
            return Err(Malformed("an attribute is longer than its contents"));
        }
    }
    Ok(field)
}

/// A method's declaration, its body still to be read.
struct Declaration {
    method: ClassMethod,
    has_code: bool,
}

fn read_method(r: &mut Cursor<'_>, pool: &Pool<'_>) -> Result<Declaration, ClassReadError> {
    let mut method = ClassMethod {
        access: r.u2()?,
        name: pool.utf8(r.u2()?)?,
        desc: pool.utf8(r.u2()?)?,
        signature: None,
        exceptions: Vec::new(),
        visible_annotations: Vec::new(),
        invisible_annotations: Vec::new(),
        visible_parameter_annotations: None,
        invisible_parameter_annotations: None,
        annotation_default: None,
        deprecated: false,
        code: None,
    };
    let mut has_code = false;
    for _ in 0..r.u2()? {
        let attribute = pool.utf8(r.u2()?)?;
        let length = r.u4()? as usize;
        let body = r.take(length)?;
        let mut a = Cursor { bytes: body, at: 0 };
        match attribute.as_str() {
            "Code" => {
                has_code = true;
                check_code_attributes(&mut a, pool)?;
            }
            "Signature" => method.signature = Some(pool.utf8(a.u2()?)?),
            "Exceptions" => {
                for _ in 0..a.u2()? {
                    method.exceptions.push(pool.class(a.u2()?)?);
                }
            }
            "RuntimeVisibleAnnotations" => {
                method.visible_annotations = read_annotations(&mut a, pool)?
            }
            "RuntimeInvisibleAnnotations" => {
                method.invisible_annotations = read_annotations(&mut a, pool)?
            }
            "RuntimeVisibleParameterAnnotations" => {
                method.visible_parameter_annotations =
                    Some(read_parameter_annotations(&mut a, pool)?)
            }
            "RuntimeInvisibleParameterAnnotations" => {
                method.invisible_parameter_annotations =
                    Some(read_parameter_annotations(&mut a, pool)?)
            }
            "AnnotationDefault" => {
                method.annotation_default = Some(read_element_value(&mut a, pool)?)
            }
            "Deprecated" => method.deprecated = true,
            "Synthetic" => method.access |= ACC_SYNTHETIC,
            _ => return Err(ClassReadError::UnsupportedAttribute(attribute)),
        }
        if !a.at_end() {
            return Err(Malformed("an attribute is longer than its contents"));
        }
    }
    Ok(Declaration { method, has_code })
}

/// Refuse a `Code` attribute carrying a table [`MethodNode`] does not read: the copy would lose it.
/// Frames are recomputed, and the line and local tables are read with the body.
fn check_code_attributes(a: &mut Cursor<'_>, pool: &Pool<'_>) -> Result<(), ClassReadError> {
    a.take(4)?; // max_stack, max_locals
    let code_length = a.u4()? as usize;
    a.take(code_length)?;
    let handlers = usize::from(a.u2()?);
    a.take(handlers * 8)?;
    for _ in 0..a.u2()? {
        let attribute = pool.utf8(a.u2()?)?;
        let length = a.u4()? as usize;
        a.take(length)?;
        if !matches!(
            attribute.as_str(),
            "StackMapTable" | "LineNumberTable" | "LocalVariableTable"
        ) {
            return Err(ClassReadError::UnsupportedAttribute(attribute));
        }
    }
    Ok(())
}

fn read_parameter_annotations(
    a: &mut Cursor<'_>,
    pool: &Pool<'_>,
) -> Result<Vec<Vec<Annotation>>, ClassReadError> {
    (0..a.u1()?).map(|_| read_annotations(a, pool)).collect()
}

fn read_annotations(
    a: &mut Cursor<'_>,
    pool: &Pool<'_>,
) -> Result<Vec<Annotation>, ClassReadError> {
    (0..a.u2()?).map(|_| read_annotation(a, pool)).collect()
}

fn read_annotation(a: &mut Cursor<'_>, pool: &Pool<'_>) -> Result<Annotation, ClassReadError> {
    let desc = pool.utf8(a.u2()?)?;
    let mut values = Vec::new();
    for _ in 0..a.u2()? {
        let name = pool.utf8(a.u2()?)?;
        values.push((name, read_element_value(a, pool)?));
    }
    Ok(Annotation { desc, values })
}

fn read_element_value(a: &mut Cursor<'_>, pool: &Pool<'_>) -> Result<ElementValue, ClassReadError> {
    let tag = a.u1()?;
    Ok(match tag {
        b'B' | b'C' | b'I' | b'S' | b'Z' => ElementValue::Int(tag, pool.integer(a.u2()?)?),
        b'J' => match pool.constant_value(a.u2()?)? {
            Constant::Long(value) => ElementValue::Long(value),
            _ => return Err(Malformed("a long element names another constant")),
        },
        b'F' => match pool.constant_value(a.u2()?)? {
            Constant::Float(bits) => ElementValue::Float(bits),
            _ => return Err(Malformed("a float element names another constant")),
        },
        b'D' => match pool.constant_value(a.u2()?)? {
            Constant::Double(bits) => ElementValue::Double(bits),
            _ => return Err(Malformed("a double element names another constant")),
        },
        b's' => ElementValue::String(
            utf8_value(pool.0, a.u2()?).ok_or(Malformed("a string element has no text"))?,
        ),
        b'e' => {
            let desc = pool.utf8(a.u2()?)?;
            ElementValue::Enum(desc, pool.utf8(a.u2()?)?)
        }
        b'c' => ElementValue::Class(pool.utf8(a.u2()?)?),
        b'@' => ElementValue::Annotation(read_annotation(a, pool)?),
        b'[' => ElementValue::Array(
            (0..a.u2()?)
                .map(|_| read_element_value(a, pool))
                .collect::<Result<_, _>>()?,
        ),
        _ => return Err(Malformed("an unknown element-value tag")),
    })
}
