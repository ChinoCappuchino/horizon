//! Field and method descriptors (JVMS 4.3): the types of values.

/// How a value is held: the JVM's computational types. `boolean`, `byte`,
/// `char`, `short` and `int` are all `Int`, as on the JVM's stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Int,
    Long,
    Float,
    Double,
    Ref,
}

impl Kind {
    /// The kind of a field descriptor (`I`, `Ljava/lang/String;`, `[I`...).
    pub fn of(descriptor: &str) -> Option<Kind> {
        Some(match descriptor.as_bytes().first()? {
            b'Z' | b'B' | b'C' | b'S' | b'I' => Kind::Int,
            b'J' => Kind::Long,
            b'F' => Kind::Float,
            b'D' => Kind::Double,
            b'L' | b'[' => Kind::Ref,
            _ => return None,
        })
    }
    /// Long and double take two local variable slots.
    pub fn wide(self) -> bool {
        matches!(self, Kind::Long | Kind::Double)
    }
}

/// A method descriptor: its parameters' descriptors and its return
/// descriptor (`V` for none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method {
    pub params: Vec<String>,
    pub ret: String,
}

impl Method {
    pub fn parse(descriptor: &str) -> Option<Method> {
        let rest = descriptor.strip_prefix('(')?;
        let (args, ret) = rest.split_once(')')?;
        let mut params = Vec::new();
        let bytes = args.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let start = i;
            while bytes[i] == b'[' {
                i += 1;
            }
            if bytes[i] == b'L' {
                i += args[i..].find(';')? + 1;
            } else {
                i += 1;
            }
            params.push(args[start..i].to_owned());
        }
        Some(Method {
            params,
            ret: ret.to_owned(),
        })
    }
    pub fn param_kinds(&self) -> Vec<Kind> {
        self.params.iter().filter_map(|p| Kind::of(p)).collect()
    }
    pub fn return_kind(&self) -> Option<Kind> {
        Kind::of(&self.ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_descriptors_split_into_their_types() {
        let m = Method::parse("(I[JLjava/lang/String;[[Lx/Y;D)Ljava/util/List;").unwrap();
        assert_eq!(m.params, ["I", "[J", "Ljava/lang/String;", "[[Lx/Y;", "D"]);
        assert_eq!(
            m.param_kinds(),
            [Kind::Int, Kind::Ref, Kind::Ref, Kind::Ref, Kind::Double]
        );
        assert_eq!(m.return_kind(), Some(Kind::Ref));
        assert_eq!(Method::parse("()V").unwrap().return_kind(), None);
    }
}
