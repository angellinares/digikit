//! ColdFire instruction rate of the emulated machine.
//!
//! The core retires one instruction per bus tick by default, so a guest second is
//! [`BASE_IPS`] instructions (the 132 MHz bus; the real core runs at 264 MHz).
//! `DIGI_CF_IPS` changes the number of instructions per emulated second to test whether
//! firmware behaviour depends on the rate. Timers, SSI pacing and scenario instruction
//! counts follow it; the bus clock (`pit::F_BUS`) does not.

/// Instructions per emulated second when `DIGI_CF_IPS` is unset (and the rate every
/// ready snapshot was taken at).
pub const BASE_IPS: u64 = 132_000_000;

/// Parse a `DIGI_CF_IPS` value: a decimal integer from 1 000 000 to 4 000 000 000.
pub fn parse_ips(text: &str) -> Result<u64, String> {
    let value: u64 = text
        .trim()
        .parse()
        .map_err(|_| format!("DIGI_CF_IPS must be an integer, got {text:?}"))?;
    if !(1_000_000..=4_000_000_000).contains(&value) {
        return Err(format!(
            "DIGI_CF_IPS must be between 1000000 and 4000000000, got {value}"
        ));
    }
    Ok(value)
}

/// Instructions per emulated second, read once from `DIGI_CF_IPS` (native builds only).
/// Panics on an invalid value so a typo cannot silently run the default rate.
pub fn cf_ips() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static IPS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *IPS.get_or_init(|| match std::env::var("DIGI_CF_IPS") {
            Ok(text) => parse_ips(&text).unwrap_or_else(|e| panic!("{e}")),
            Err(_) => BASE_IPS,
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        BASE_IPS
    }
}

/// `n` instructions authored at [`BASE_IPS`] (emulated time) expressed at `ips`.
pub fn scale_instr(n: u64, ips: u64) -> u64 {
    if ips == BASE_IPS {
        return n;
    }
    ((n as u128 * ips as u128 + BASE_IPS as u128 / 2) / BASE_IPS as u128) as u64
}

/// Inverse of [`scale_instr`]: `n` instructions counted at `ips` expressed at [`BASE_IPS`].
pub fn unscale_instr(n: u64, ips: u64) -> u64 {
    if ips == BASE_IPS {
        return n;
    }
    ((n as u128 * BASE_IPS as u128 + ips as u128 / 2) / ips as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_validates() {
        assert_eq!(parse_ips("66000000"), Ok(66_000_000));
        assert!(parse_ips("abc").is_err());
        assert!(parse_ips("0").is_err());
        assert!(parse_ips("9999999999").is_err());
    }

    #[test]
    fn scale_keeps_emulated_time() {
        assert_eq!(scale_instr(1_234_567, BASE_IPS), 1_234_567);
        assert_eq!(scale_instr(132_000_000, 264_000_000), 264_000_000);
        assert_eq!(scale_instr(132_000_000, 33_000_000), 33_000_000);
        assert_eq!(scale_instr(3, 66_000_000), 2); // 1.5 rounds up
        assert_eq!(
            unscale_instr(scale_instr(1_000_000, 198_000_000), 198_000_000),
            1_000_000
        );
    }
}
