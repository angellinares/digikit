//! A small JSON reader and writer for the rig's request and response (std only, like the rest of this crate).
//! Numbers are integers: the rig's values are addresses, words and counts.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn int(&self) -> Result<i64, String> {
        match self {
            Json::Int(v) => Ok(*v),
            other => Err(format!("expected an integer, got {other:?}")),
        }
    }

    pub fn str(&self) -> Result<&str, String> {
        match self {
            Json::Str(s) => Ok(s),
            other => Err(format!("expected a string, got {other:?}")),
        }
    }

    pub fn arr(&self) -> Result<&[Json], String> {
        match self {
            Json::Arr(a) => Ok(a),
            other => Err(format!("expected an array, got {other:?}")),
        }
    }

    pub fn obj(&self) -> Result<&[(String, Json)], String> {
        match self {
            Json::Obj(o) => Ok(o),
            other => Err(format!("expected an object, got {other:?}")),
        }
    }

    pub fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(v) => {
                let _ = write!(out, "{v}");
            }
            Json::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(out, "\\u{:04x}", c as u32);
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Obj(items) => {
                out.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    Json::Str(k.clone()).write(out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

pub fn parse(text: &str) -> Result<Json, String> {
    let mut p = Parser {
        b: text.as_bytes(),
        at: 0,
    };
    let v = p.value()?;
    p.space();
    if p.at != p.b.len() {
        return Err(p.fail("trailing text"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn fail(&self, what: &str) -> String {
        format!("json: {what} at byte {}", self.at)
    }

    fn space(&mut self) {
        while self.b.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.space();
        if self.b.get(self.at) == Some(&c) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.fail(&format!("expected '{}'", c as char)))
        }
    }

    fn word(&mut self, w: &str, v: Json) -> Result<Json, String> {
        if self.b[self.at..].starts_with(w.as_bytes()) {
            self.at += w.len();
            Ok(v)
        } else {
            Err(self.fail("bad literal"))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.space();
        match self.b.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.b.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Json::Obj(items));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.eat(b':')?;
                    items.push((key, self.value()?));
                    self.space();
                    match self.b.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Json::Obj(items));
                        }
                        _ => return Err(self.fail("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.b.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.space();
                    match self.b.get(self.at) {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(self.fail("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'n') => self.word("null", Json::Null),
            Some(c) if c.is_ascii_digit() || *c == b'-' => {
                let start = self.at;
                self.at += 1;
                while self.b.get(self.at).is_some_and(u8::is_ascii_digit) {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.b[start..self.at]).unwrap_or("");
                text.parse::<i64>()
                    .map(Json::Int)
                    .map_err(|_| self.fail("not an integer"))
            }
            _ => Err(self.fail("unexpected input")),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.b.get(self.at) != Some(&b'"') {
            return Err(self.fail("expected a string"));
        }
        self.at += 1;
        let mut out = Vec::new();
        loop {
            match self.b.get(self.at) {
                None => return Err(self.fail("unterminated string")),
                Some(b'"') => {
                    self.at += 1;
                    return String::from_utf8(out).map_err(|_| self.fail("bad utf-8"));
                }
                Some(b'\\') => {
                    self.at += 1;
                    let c = *self.b.get(self.at).ok_or_else(|| self.fail("bad escape"))?;
                    self.at += 1;
                    out.push(match c {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        b'"' | b'\\' | b'/' => c,
                        _ => return Err(self.fail("unsupported escape")),
                    });
                }
                Some(&c) => {
                    out.push(c);
                    self.at += 1;
                }
            }
        }
    }
}
