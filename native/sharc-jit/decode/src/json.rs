//! A minimal, hand-written JSON reader for `tools/sharcspec/decode_table.json`
//! (no `serde`, no crate dependencies at all -- see the crate's top-level
//! docs). It implements just enough of JSON to read that one table: objects,
//! arrays, strings (with the escapes JSON allows), booleans, `null`, and
//! integers (this table never uses a JSON float). Object keys keep their
//! source order in a `Vec`, not a hash map, so nothing here depends on
//! hashing order.

use std::fmt;

#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<Value>),
    /// Key/value pairs in source order.
    Obj(Vec<(String, Value)>),
}

#[derive(Debug, Clone)]
pub struct JsonError(pub String);

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON error: {}", self.0)
    }
}

impl std::error::Error for JsonError {}

impl Value {
    /// Part of this reader's general `Value` API, used by callers outside
    /// this crate's own two consumers (`isa.rs`, and `examples/` via
    /// `#[path]`) even though neither of those happens to need it today.
    #[allow(dead_code)]
    pub fn as_obj(&self) -> Result<&[(String, Value)], JsonError> {
        match self {
            Value::Obj(kv) => Ok(kv),
            _ => Err(JsonError("expected object".into())),
        }
    }

    pub fn as_arr(&self) -> Result<&[Value], JsonError> {
        match self {
            Value::Arr(v) => Ok(v),
            _ => Err(JsonError("expected array".into())),
        }
    }

    pub fn as_str(&self) -> Result<&str, JsonError> {
        match self {
            Value::Str(s) => Ok(s),
            _ => Err(JsonError("expected string".into())),
        }
    }

    pub fn as_int(&self) -> Result<i64, JsonError> {
        match self {
            Value::Int(n) => Ok(*n),
            _ => Err(JsonError("expected integer".into())),
        }
    }

    #[allow(dead_code)]
    pub fn as_bool(&self) -> Result<bool, JsonError> {
        match self {
            Value::Bool(b) => Ok(*b),
            _ => Err(JsonError("expected bool".into())),
        }
    }

    /// Field lookup within an object; None if this isn't an object or the
    /// key is absent (mirrors Python's `dict.get`).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

pub fn parse(text: &str) -> Result<Value, JsonError> {
    let bytes = text.as_bytes();
    let mut p = Parser { b: bytes, i: 0 };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    if p.i != p.b.len() {
        return Err(JsonError(format!("trailing data at byte {}", p.i)));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), JsonError> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(JsonError(format!(
                "expected '{}' at byte {}",
                c as char, self.i
            )))
        }
    }

    fn parse_value(&mut self) -> Result<Value, JsonError> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(Value::Str(self.parse_string()?)),
            Some(b't') => {
                self.expect_lit("true")?;
                Ok(Value::Bool(true))
            }
            Some(b'f') => {
                self.expect_lit("false")?;
                Ok(Value::Bool(false))
            }
            Some(b'n') => {
                self.expect_lit("null")?;
                Ok(Value::Null)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            _ => Err(JsonError(format!("unexpected byte at {}", self.i))),
        }
    }

    fn expect_lit(&mut self, lit: &str) -> Result<(), JsonError> {
        let end = self.i + lit.len();
        if end <= self.b.len() && &self.b[self.i..end] == lit.as_bytes() {
            self.i = end;
            Ok(())
        } else {
            Err(JsonError(format!("expected {lit} at byte {}", self.i)))
        }
    }

    fn parse_object(&mut self) -> Result<Value, JsonError> {
        self.expect(b'{')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Value::Obj(out));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let value = self.parse_value()?;
            out.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                _ => return Err(JsonError(format!("expected ',' or '}}' at {}", self.i))),
            }
        }
        Ok(Value::Obj(out))
    }

    fn parse_array(&mut self) -> Result<Value, JsonError> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Arr(out));
        }
        loop {
            let value = self.parse_value()?;
            out.push(value);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    break;
                }
                _ => return Err(JsonError(format!("expected ',' or ']' at {}", self.i))),
            }
        }
        Ok(Value::Arr(out))
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = self
                .peek()
                .ok_or_else(|| JsonError("unterminated string".into()))?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let esc = self
                        .peek()
                        .ok_or_else(|| JsonError("unterminated escape".into()))?;
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            if self.i + 4 > self.b.len() {
                                return Err(JsonError("truncated \\u escape".into()));
                            }
                            let hex = std::str::from_utf8(&self.b[self.i..self.i + 4])
                                .map_err(|_| JsonError("bad \\u escape".into()))?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| JsonError("bad \\u escape".into()))?;
                            self.i += 4;
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                            }
                        }
                        _ => return Err(JsonError(format!("bad escape '\\{}'", esc as char))),
                    }
                }
                _ => {
                    // Reassemble UTF-8 continuation bytes verbatim.
                    let start = self.i - 1;
                    let mut end = self.i;
                    while end < self.b.len() && self.b[end] & 0xC0 == 0x80 {
                        end += 1;
                    }
                    self.i = end;
                    let s = std::str::from_utf8(&self.b[start..end])
                        .map_err(|_| JsonError("invalid utf-8 in string".into()))?;
                    out.push_str(s);
                }
            }
        }
        Ok(out)
    }

    fn parse_number(&mut self) -> Result<Value, JsonError> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if matches!(self.peek(), Some(b'.') | Some(b'e') | Some(b'E')) {
            return Err(JsonError(
                "non-integer JSON number is not supported by this reader".into(),
            ));
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).unwrap();
        text.parse::<i64>()
            .map(Value::Int)
            .map_err(|e| JsonError(format!("bad number {text:?}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars() {
        assert!(matches!(parse("null").unwrap(), Value::Null));
        assert!(matches!(parse("true").unwrap(), Value::Bool(true)));
        assert!(matches!(parse("false").unwrap(), Value::Bool(false)));
        assert!(matches!(parse("42").unwrap(), Value::Int(42)));
        assert!(matches!(parse("-7").unwrap(), Value::Int(-7)));
    }

    #[test]
    fn parses_string_escapes() {
        let v = parse(r#""a\"b\\c\nA""#).unwrap();
        assert_eq!(v.as_str().unwrap(), "a\"b\\c\nA");
    }

    #[test]
    fn parses_object_and_array_preserving_order() {
        let v = parse(r#"{"b": 1, "a": [1, 2, 3], "c": true}"#).unwrap();
        let obj = v.as_obj().unwrap();
        let keys: Vec<&str> = obj.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["b", "a", "c"]);
        assert_eq!(v.get("a").unwrap().as_arr().unwrap().len(), 3);
    }

    #[test]
    fn rejects_trailing_data() {
        assert!(parse("1 2").is_err());
    }
}
