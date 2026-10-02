//! Decode a contiguous short-word fixture for parity checks.
//!
//! Usage: `sharc-decode [--pc N] WORD...`, where words are decimal or `0x`-
//! prefixed hexadecimal u16 values.  It is deliberately image-agnostic; callers extract
//! a bounded fixture from their own loader-backed memory first.

use sharc_native::decode::decode_at;

fn parse_u32(text: &str) -> Result<u32, String> {
    let (text, radix) = text.strip_prefix("0x").map_or((text, 10), |hex| (hex, 16));
    u32::from_str_radix(text, radix).map_err(|_| format!("invalid integer {text:?}"))
}

fn main() -> Result<(), String> {
    let mut pc = 0;
    let mut words = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--pc" {
            pc = parse_u32(&args.next().ok_or("--pc needs a value")?)?;
        } else {
            let value = parse_u32(&arg)?;
            words.push(u16::try_from(value).map_err(|_| format!("word out of range: {arg}"))?);
        }
    }
    let decoded = decode_at(|sw| words.get(sw as usize).copied(), pc);
    print!(
        "{}|{}|{}|{:?}",
        decoded.type_name,
        decoded.length_bytes.map_or(-1, i16::from),
        decoded.kind.as_str(),
        decoded.note
    );
    for field in decoded.fields() {
        print!(
            "|{}:{}:{}:{}:{}",
            field.key, field.stem, field.hi, field.lo, field.value
        );
    }
    println!();
    Ok(())
}
