//! The JVM class file format (JVMS chapter 4): the constant pool, fields,
//! methods and the attributes the compiler uses. Other attributes are kept as
//! their name and bytes.

/// Why a class file could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a class file (no `0xCAFEBABE`).
    Magic,
    /// The data ended inside a structure.
    Truncated,
    /// A constant pool tag the format doesn't have.
    Tag(u8),
    /// A constant pool index out of range, or of the wrong kind.
    Index(u16),
    /// Something else wrong, said in words.
    Malformed(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Magic => f.write_str("not a class file"),
            Error::Truncated => f.write_str("the class file ends early"),
            Error::Tag(t) => write!(f, "constant pool tag {t} is unknown"),
            Error::Index(i) => write!(f, "constant pool index {i} is wrong"),
            Error::Malformed(why) => f.write_str(why),
        }
    }
}
impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// One constant pool entry. Long and Double take two slots: the second is
/// `Unusable`, as is slot 0.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Unusable,
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(u16),
    String(u16),
    Fieldref(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
    NameAndType(u16, u16),
    MethodHandle(u8, u16),
    MethodType(u16),
    Dynamic(u16, u16),
    InvokeDynamic(u16, u16),
    Module(u16),
    Package(u16),
}

/// A field or a method.
#[derive(Clone, Debug)]
pub struct Member {
    pub access: u16,
    pub name: String,
    pub descriptor: String,
    pub attributes: Vec<Attribute>,
}

impl Member {
    /// The method's code, if it has some (not abstract or native).
    pub fn code(&self) -> Option<&Code> {
        self.attributes.iter().find_map(|a| match a {
            Attribute::Code(c) => Some(c),
            _ => None,
        })
    }

    /// The member's annotations, visible and invisible at run time.
    pub fn annotations(&self) -> impl Iterator<Item = &Annotation> {
        self.attributes.iter().flat_map(|a| match a {
            Attribute::Annotations(_, v) => v.as_slice(),
            _ => &[],
        })
    }
}

/// An exception handler: `[start, end)` handled at `handler`; `catch_type`
/// 0 catches everything (`finally`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handler {
    pub start: u16,
    pub end: u16,
    pub handler: u16,
    pub catch_type: u16,
}

#[derive(Clone, Debug)]
pub struct Code {
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: Vec<u8>,
    pub handlers: Vec<Handler>,
    pub attributes: Vec<Attribute>,
}

/// An annotation: its type descriptor and its named values.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub type_descriptor: String,
    pub values: Vec<(String, ElementValue)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ElementValue {
    /// A constant: its tag (`B C D F I J S Z s`) and constant pool index.
    Const(u8, u16),
    Enum {
        type_descriptor: String,
        name: String,
    },
    Class(String),
    Annotation(Annotation),
    Array(Vec<ElementValue>),
}

#[derive(Clone, Debug)]
pub struct BootstrapMethod {
    pub method_handle: u16,
    pub arguments: Vec<u16>,
}

#[derive(Clone, Debug)]
pub enum Attribute {
    Code(Code),
    ConstantValue(u16),
    Exceptions(Vec<u16>),
    Signature(String),
    SourceFile(String),
    BootstrapMethods(Vec<BootstrapMethod>),
    /// Runtime-visible (`true`) or invisible (`false`, where Mixin's
    /// annotations are) annotations.
    Annotations(bool, Vec<Annotation>),
    InnerClasses(Vec<[u16; 4]>),
    /// Any other attribute, as its name and bytes.
    Other(String, Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct ClassFile {
    pub minor: u16,
    pub major: u16,
    pub constants: Vec<Constant>,
    pub access: u16,
    pub this_class: u16,
    pub super_class: u16,
    pub interfaces: Vec<u16>,
    pub fields: Vec<Member>,
    pub methods: Vec<Member>,
    pub attributes: Vec<Attribute>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or(Error::Truncated)?;
        let out = self.data.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn done(&self) -> bool {
        self.at == self.data.len()
    }
}

/// Java's "modified UTF-8" (JVMS 4.4.7): NUL as two bytes, and characters
/// outside the basic plane as two encoded surrogates.
pub fn modified_utf8(bytes: &[u8]) -> Result<String> {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (unit, len) = if b & 0x80 == 0 {
            (u16::from(b), 1)
        } else if b & 0xE0 == 0xC0 {
            let c = *bytes.get(i + 1).ok_or(Error::Malformed("modified UTF-8"))?;
            ((u16::from(b & 0x1F) << 6) | u16::from(c & 0x3F), 2)
        } else if b & 0xF0 == 0xE0 {
            let c = *bytes.get(i + 1).ok_or(Error::Malformed("modified UTF-8"))?;
            let d = *bytes.get(i + 2).ok_or(Error::Malformed("modified UTF-8"))?;
            (
                (u16::from(b & 0x0F) << 12) | (u16::from(c & 0x3F) << 6) | u16::from(d & 0x3F),
                3,
            )
        } else {
            return Err(Error::Malformed("modified UTF-8"));
        };
        units.push(unit);
        i += len;
    }
    // Unpaired surrogates (legal in Java strings) become U+FFFD.
    Ok(char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect())
}

impl ClassFile {
    pub fn parse(data: &[u8]) -> Result<ClassFile> {
        let mut r = Reader { data, at: 0 };
        if r.u32()? != 0xCAFE_BABE {
            return Err(Error::Magic);
        }
        let minor = r.u16()?;
        let major = r.u16()?;
        let count = r.u16()?;
        let mut constants = vec![Constant::Unusable];
        while constants.len() < usize::from(count) {
            let tag = r.u8()?;
            let c = match tag {
                1 => {
                    let len = r.u16()?;
                    Constant::Utf8(modified_utf8(r.bytes(usize::from(len))?)?)
                }
                3 => Constant::Integer(r.u32()? as i32),
                4 => Constant::Float(f32::from_bits(r.u32()?)),
                5 | 6 => {
                    let hi = u64::from(r.u32()?);
                    let v = (hi << 32) | u64::from(r.u32()?);
                    constants.push(if tag == 5 {
                        Constant::Long(v as i64)
                    } else {
                        Constant::Double(f64::from_bits(v))
                    });
                    Constant::Unusable
                }
                7 => Constant::Class(r.u16()?),
                8 => Constant::String(r.u16()?),
                9 => Constant::Fieldref(r.u16()?, r.u16()?),
                10 => Constant::Methodref(r.u16()?, r.u16()?),
                11 => Constant::InterfaceMethodref(r.u16()?, r.u16()?),
                12 => Constant::NameAndType(r.u16()?, r.u16()?),
                15 => Constant::MethodHandle(r.u8()?, r.u16()?),
                16 => Constant::MethodType(r.u16()?),
                17 => Constant::Dynamic(r.u16()?, r.u16()?),
                18 => Constant::InvokeDynamic(r.u16()?, r.u16()?),
                19 => Constant::Module(r.u16()?),
                20 => Constant::Package(r.u16()?),
                t => return Err(Error::Tag(t)),
            };
            constants.push(c);
        }
        let mut class = ClassFile {
            minor,
            major,
            constants,
            access: 0,
            this_class: 0,
            super_class: 0,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attributes: Vec::new(),
        };
        class.access = r.u16()?;
        class.this_class = r.u16()?;
        class.super_class = r.u16()?;
        for _ in 0..r.u16()? {
            class.interfaces.push(r.u16()?);
        }
        for _ in 0..r.u16()? {
            let m = class.member(&mut r)?;
            class.fields.push(m);
        }
        for _ in 0..r.u16()? {
            let m = class.member(&mut r)?;
            class.methods.push(m);
        }
        class.attributes = class.attributes(&mut r)?;
        if !r.done() {
            return Err(Error::Malformed("bytes after the class's attributes"));
        }
        Ok(class)
    }

    fn member(&self, r: &mut Reader<'_>) -> Result<Member> {
        let access = r.u16()?;
        let name = self.utf8(r.u16()?)?.to_owned();
        let descriptor = self.utf8(r.u16()?)?.to_owned();
        let attributes = self.attributes(r)?;
        Ok(Member {
            access,
            name,
            descriptor,
            attributes,
        })
    }

    fn attributes(&self, r: &mut Reader<'_>) -> Result<Vec<Attribute>> {
        let count = r.u16()?;
        let mut out = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let name = self.utf8(r.u16()?)?;
            let len = r.u32()? as usize;
            let body = r.bytes(len)?;
            let mut b = Reader { data: body, at: 0 };
            let a = match name {
                "Code" => {
                    let max_stack = b.u16()?;
                    let max_locals = b.u16()?;
                    let len = b.u32()? as usize;
                    let code = b.bytes(len)?.to_vec();
                    let mut handlers = Vec::new();
                    for _ in 0..b.u16()? {
                        handlers.push(Handler {
                            start: b.u16()?,
                            end: b.u16()?,
                            handler: b.u16()?,
                            catch_type: b.u16()?,
                        });
                    }
                    let attributes = self.attributes(&mut b)?;
                    Attribute::Code(Code {
                        max_stack,
                        max_locals,
                        code,
                        handlers,
                        attributes,
                    })
                }
                "ConstantValue" => Attribute::ConstantValue(b.u16()?),
                "Exceptions" => {
                    let mut v = Vec::new();
                    for _ in 0..b.u16()? {
                        v.push(b.u16()?);
                    }
                    Attribute::Exceptions(v)
                }
                "Signature" => Attribute::Signature(self.utf8(b.u16()?)?.to_owned()),
                "SourceFile" => Attribute::SourceFile(self.utf8(b.u16()?)?.to_owned()),
                "BootstrapMethods" => {
                    let mut v = Vec::new();
                    for _ in 0..b.u16()? {
                        let method_handle = b.u16()?;
                        let mut arguments = Vec::new();
                        for _ in 0..b.u16()? {
                            arguments.push(b.u16()?);
                        }
                        v.push(BootstrapMethod {
                            method_handle,
                            arguments,
                        });
                    }
                    Attribute::BootstrapMethods(v)
                }
                "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                    let mut v = Vec::new();
                    for _ in 0..b.u16()? {
                        v.push(self.annotation(&mut b)?);
                    }
                    Attribute::Annotations(name == "RuntimeVisibleAnnotations", v)
                }
                "InnerClasses" => {
                    let mut v = Vec::new();
                    for _ in 0..b.u16()? {
                        v.push([b.u16()?, b.u16()?, b.u16()?, b.u16()?]);
                    }
                    Attribute::InnerClasses(v)
                }
                _ => {
                    b.at = len;
                    Attribute::Other(name.to_owned(), body.to_vec())
                }
            };
            if !b.done() {
                return Err(Error::Malformed("an attribute longer than its contents"));
            }
            out.push(a);
        }
        Ok(out)
    }

    fn annotation(&self, r: &mut Reader<'_>) -> Result<Annotation> {
        let type_descriptor = self.utf8(r.u16()?)?.to_owned();
        let mut values = Vec::new();
        for _ in 0..r.u16()? {
            let name = self.utf8(r.u16()?)?.to_owned();
            values.push((name, self.element_value(r)?));
        }
        Ok(Annotation {
            type_descriptor,
            values,
        })
    }

    fn element_value(&self, r: &mut Reader<'_>) -> Result<ElementValue> {
        let tag = r.u8()?;
        Ok(match tag {
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' => {
                ElementValue::Const(tag, r.u16()?)
            }
            b'e' => ElementValue::Enum {
                type_descriptor: self.utf8(r.u16()?)?.to_owned(),
                name: self.utf8(r.u16()?)?.to_owned(),
            },
            b'c' => ElementValue::Class(self.utf8(r.u16()?)?.to_owned()),
            b'@' => ElementValue::Annotation(self.annotation(r)?),
            b'[' => {
                let mut v = Vec::new();
                for _ in 0..r.u16()? {
                    v.push(self.element_value(r)?);
                }
                ElementValue::Array(v)
            }
            _ => return Err(Error::Malformed("an annotation value with an unknown tag")),
        })
    }

    /// The text of a Utf8 constant.
    pub fn utf8(&self, index: u16) -> Result<&str> {
        match self.constants.get(usize::from(index)) {
            Some(Constant::Utf8(s)) => Ok(s),
            _ => Err(Error::Index(index)),
        }
    }

    /// The internal name (`net/minecraft/world/item/Item`) of a Class constant.
    pub fn class_name(&self, index: u16) -> Result<&str> {
        match self.constants.get(usize::from(index)) {
            Some(Constant::Class(name)) => self.utf8(*name),
            _ => Err(Error::Index(index)),
        }
    }

    /// This class's internal name.
    pub fn name(&self) -> Result<&str> {
        self.class_name(self.this_class)
    }

    /// The superclass's internal name; none for `java/lang/Object` and modules.
    pub fn super_name(&self) -> Result<Option<&str>> {
        if self.super_class == 0 {
            return Ok(None);
        }
        self.class_name(self.super_class).map(Some)
    }

    /// A field or method reference: (owner, name, descriptor).
    pub fn member_ref(&self, index: u16) -> Result<(&str, &str, &str)> {
        match self.constants.get(usize::from(index)) {
            Some(
                Constant::Fieldref(class, nat)
                | Constant::Methodref(class, nat)
                | Constant::InterfaceMethodref(class, nat),
            ) => {
                let (name, descriptor) = self.name_and_type(*nat)?;
                Ok((self.class_name(*class)?, name, descriptor))
            }
            _ => Err(Error::Index(index)),
        }
    }

    pub fn name_and_type(&self, index: u16) -> Result<(&str, &str)> {
        match self.constants.get(usize::from(index)) {
            Some(Constant::NameAndType(name, descriptor)) => {
                Ok((self.utf8(*name)?, self.utf8(*descriptor)?))
            }
            _ => Err(Error::Index(index)),
        }
    }

    /// The class's annotations, visible and invisible.
    pub fn annotations(&self) -> impl Iterator<Item = &Annotation> {
        self.attributes.iter().flat_map(|a| match a {
            Attribute::Annotations(_, v) => v.as_slice(),
            _ => &[],
        })
    }

    pub fn bootstrap_methods(&self) -> &[BootstrapMethod] {
        self.attributes
            .iter()
            .find_map(|a| match a {
                Attribute::BootstrapMethods(v) => Some(v.as_slice()),
                _ => None,
            })
            .unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_utf8_has_its_two_byte_nul_and_surrogate_pairs() {
        assert_eq!(modified_utf8(b"abc").unwrap(), "abc");
        assert_eq!(modified_utf8(&[0xC0, 0x80]).unwrap(), "\0");
        // U+1F98A (a fox) as two encoded surrogates, D83E DD8A.
        let fox = [0xED, 0xA0, 0xBE, 0xED, 0xB6, 0x8A];
        assert_eq!(modified_utf8(&fox).unwrap(), "\u{1F98A}");
        assert!(modified_utf8(&[0xC3]).is_err());
    }

    #[test]
    fn not_a_class_file_and_a_cut_one_are_refused() {
        assert_eq!(ClassFile::parse(b"PK\x03\x04").unwrap_err(), Error::Magic);
        assert_eq!(
            ClassFile::parse(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0]).unwrap_err(),
            Error::Truncated
        );
    }
}
