//! Tokens of the Rust subset the transpiled core and the runtime are
//! written in (tools/sharc_transpile.py output, native/sharc/src/rt*.rs).

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    /// An integer literal: value and suffix ("" when none).
    Int(u128, String),
    /// A float literal: its text (without suffix) and suffix.
    Float(String, String),
    Str(String),
    Char(char),
    Lifetime(String),
    /// Punctuation, longest match first.
    P(&'static str),
    Eof,
}

const PUNCT: [&str; 44] = [
    "<<=", ">>=", "...", "..=", "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "<<",
    ">>", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "..", "+", "-", "*", "/", "%", "^",
    "!", "&", "|", "=", "<", ">", "@", ".", ",", ";", ":", "#", "$", "?",
];
const BRACKETS: [&str; 6] = ["(", ")", "[", "]", "{", "}"];

pub struct Lexed {
    pub toks: Vec<Tok>,
    /// Byte offset of each token (for error messages).
    pub pos: Vec<usize>,
}

pub fn lex(src: &str) -> Result<Lexed, String> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut toks = Vec::new();
    let mut pos = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let mut depth = 0;
            while i + 1 < b.len() {
                if b[i] == b'/' && b[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        pos.push(i);
        if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            // r#ident
            if &src[s..i] == "r" && i < b.len() && b[i] == b'#' {
                i += 1;
                let s2 = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                toks.push(Tok::Ident(src[s2..i].to_string()));
                continue;
            }
            toks.push(Tok::Ident(src[s..i].to_string()));
            continue;
        }
        if c.is_ascii_digit() {
            let s = i;
            let (radix, digits_start) = if c == b'0' && i + 1 < b.len() && matches!(b[i + 1], b'x' | b'b' | b'o') {
                (
                    match b[i + 1] {
                        b'x' => 16,
                        b'b' => 2,
                        _ => 8,
                    },
                    i + 2,
                )
            } else {
                (10, i)
            };
            i = digits_start;
            let mut digits = String::new();
            let mut is_float = false;
            loop {
                if i >= b.len() {
                    break;
                }
                let d = b[i];
                if d == b'_' {
                    i += 1;
                    continue;
                }
                let ok = if radix == 16 { d.is_ascii_hexdigit() } else { d.is_ascii_digit() };
                if ok {
                    digits.push(d as char);
                    i += 1;
                    continue;
                }
                if radix == 10
                    && d == b'.'
                    && !is_float
                    && i + 1 < b.len()
                    && b[i + 1].is_ascii_digit()
                {
                    is_float = true;
                    digits.push('.');
                    i += 1;
                    continue;
                }
                if radix == 10 && (d == b'e' || d == b'E') && is_float {
                    digits.push('e');
                    i += 1;
                    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
                        digits.push(b[i] as char);
                        i += 1;
                    }
                    continue;
                }
                break;
            }
            // suffix
            let ss = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let suffix = src[ss..i].trim_start_matches('_').to_string();
            if is_float || suffix == "f64" || suffix == "f32" {
                toks.push(Tok::Float(digits, suffix));
            } else {
                let v = u128::from_str_radix(&digits, radix)
                    .map_err(|e| format!("bad integer {} at {s}: {e}", &src[s..i]))?;
                toks.push(Tok::Int(v, suffix));
            }
            continue;
        }
        if c == b'"' {
            i += 1;
            let mut out = String::new();
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                    match b[i] {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'\\' => out.push('\\'),
                        b'"' => out.push('"'),
                        b'\'' => out.push('\''),
                        b'0' => out.push('\0'),
                        b'u' => {
                            // \u{XXXX}
                            let s = i + 2;
                            let e = src[s..].find('}').map(|k| s + k).ok_or("bad escape")?;
                            let v = u32::from_str_radix(&src[s..e], 16).map_err(|e| e.to_string())?;
                            out.push(char::from_u32(v).unwrap_or('?'));
                            i = e;
                        }
                        x => out.push(x as char),
                    }
                    i += 1;
                    continue;
                }
                // Copy one UTF-8 character.
                let ch = src[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
            i += 1;
            toks.push(Tok::Str(out));
            continue;
        }
        if c == b'\'' {
            // A char literal 'x' / '\n', or a lifetime 'a.
            if i + 2 < b.len() && b[i + 2] == b'\'' {
                toks.push(Tok::Char(b[i + 1] as char));
                i += 3;
                continue;
            }
            if i + 3 < b.len() && b[i + 1] == b'\\' && b[i + 3] == b'\'' {
                let ch = match b[i + 2] {
                    b'n' => '\n',
                    b't' => '\t',
                    b'0' => '\0',
                    x => x as char,
                };
                toks.push(Tok::Char(ch));
                i += 4;
                continue;
            }
            let s = i + 1;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            toks.push(Tok::Lifetime(src[s..i].to_string()));
            continue;
        }
        let rest = &src[i..];
        if let Some(p) = BRACKETS.iter().find(|p| rest.starts_with(**p)) {
            toks.push(Tok::P(p));
            i += 1;
            continue;
        }
        if let Some(p) = PUNCT.iter().find(|p| rest.starts_with(**p)) {
            toks.push(Tok::P(p));
            i += p.len();
            continue;
        }
        return Err(format!("unexpected character {:?} at byte {i}", c as char));
    }
    pos.push(b.len());
    toks.push(Tok::Eof);
    Ok(Lexed { toks, pos })
}
