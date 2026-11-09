//! `patchkit preflight PATCHED.syx`: the checks to read before an image goes to a device.
//!
//! It answers: is this a patched image whose stock base is a known firmware (`[[firmware]]` of `devices/*.toml`) that is present in
//! `firmware/`; are the bootstrap (2), updater (4), meta (5), boot data (8) and every other section except MAIN OS (3) and the DSP (7)
//! byte-identical to the base, stored and decoded; which of 3 and 7 differ; do the container checksum, the HMAC and the size limits of the
//! port hold (`verify`). It never writes anything. Any failed line makes the whole result a failure.

use crate::error::*;
use crate::firmware::Firmware;
use crate::port::Registry;
use crate::verify;

/// A stock firmware named by a device file.
#[derive(Clone, Debug)]
pub struct Known {
    pub device: String,
    pub version: String,
    pub sha256: String,
    pub filename: String,
}

/// The `[[firmware]]` tables of device files (the text of each `devices/*.toml`).
pub fn known_from_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Result<Vec<Known>> {
    let mut out = vec![];
    for t in texts {
        let v: toml::Value = toml::from_str(t)?;
        let Some(short) = v
            .get("device")
            .and_then(|d| d.get("short"))
            .and_then(|s| s.as_str())
        else {
            continue;
        };
        for f in v
            .get("firmware")
            .and_then(|f| f.as_array())
            .into_iter()
            .flatten()
        {
            let s = |k: &str| f.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            out.push(Known {
                device: short.to_string(),
                version: s("version"),
                sha256: s("sha256"),
                filename: s("filename"),
            });
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct Line {
    pub ok: bool,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub lines: Vec<Line>,
}

impl Outcome {
    fn ok(&mut self, t: impl Into<String>) {
        self.lines.push(Line {
            ok: true,
            text: t.into(),
        });
    }
    fn fail(&mut self, t: impl Into<String>) {
        self.lines.push(Line {
            ok: false,
            text: t.into(),
        });
    }
    pub fn passed(&self) -> bool {
        self.lines.iter().all(|l| l.ok)
    }
}

/// Sections that a patch may change.
const PATCHABLE: [u32; 2] = [3, 7];

/// Check `patched` against its base. `stock` returns the bytes of the base file for a known firmware (`None`: not present). `code`:
/// the DSP code ranges of the patch's plan, for the ISA check (`dspscan::scan`).
pub fn preflight(
    patched: &[u8],
    reg: &Registry,
    known: &[Known],
    stock: &dyn Fn(&Known) -> Option<Vec<u8>>,
    code: Option<&[(u64, u64)]>,
) -> Outcome {
    let mut o = Outcome::default();
    let fw = match Firmware::parse(patched) {
        Ok(f) => f,
        Err(e) => {
            o.fail(format!("not a firmware .syx: {e}"));
            return o;
        }
    };
    o.ok(format!(
        "patched image: {}, {} bytes, sha256 {}",
        fw.device.short(),
        patched.len(),
        fw.syx_sha256
    ));
    if let Some(k) = known.iter().find(|k| k.sha256 == fw.syx_sha256) {
        o.fail(format!(
            "not patched: this is the stock {} {} ({}); a patched image carries a patch record",
            k.device, k.version, k.filename
        ));
        return o;
    }
    let record = match verify::patch_record(patched) {
        Ok(Some(r)) => r,
        Ok(None) => {
            o.fail("not patched: the image has no patch record (DKPR) and is not a known stock firmware");
            return o;
        }
        Err(e) => {
            o.fail(format!("patch record unreadable: {e}"));
            return o;
        }
    };
    o.ok(format!(
        "patch record: base sha256 {}, modules {}",
        record.base_sha256,
        if record.modules.is_empty() {
            "none".into()
        } else {
            record.modules.join(", ")
        }
    ));

    let Some(k) = known.iter().find(|k| k.sha256 == record.base_sha256) else {
        o.fail(format!(
            "the base sha256 {} is not a known stock firmware (no [[firmware]] with it in devices/*.toml)",
            record.base_sha256
        ));
        return o;
    };
    o.ok(format!(
        "base is the stock {} {} (a [[firmware]] of devices/*.toml, file name {})",
        k.device, k.version, k.filename
    ));
    if k.device != fw.device.short() {
        o.fail(format!(
            "the image is for {} but its base is a {} firmware",
            fw.device.short(),
            k.device
        ));
        return o;
    }
    let Some(base_bytes) = stock(k) else {
        o.fail(format!(
            "the stock file is not in firmware/ (looked for sha256 {}, usually {})",
            k.sha256, k.filename
        ));
        return o;
    };
    let base = match Firmware::parse(&base_bytes) {
        Ok(b) if b.syx_sha256 == k.sha256 => b,
        Ok(_) => {
            o.fail("the stock file found does not have the base sha256");
            return o;
        }
        Err(e) => {
            o.fail(format!("the stock file does not parse: {e}"));
            return o;
        }
    };
    o.ok(format!(
        "stock file present in firmware/, sha256 {}",
        base.syx_sha256
    ));

    // Every section: the ids match, and all but the patchable ones are byte-identical.
    let ids = |f: &Firmware| f.sections.iter().map(|s| s.id).collect::<Vec<_>>();
    if ids(&base) != ids(&fw) {
        o.fail(format!(
            "section ids differ: base {:?}, image {:?}",
            ids(&base),
            ids(&fw)
        ));
        return o;
    }
    let name = |id: u32| match id {
        2 => "bootstrap",
        3 => "MAIN OS",
        4 => "updater",
        5 => "meta",
        7 => "DSP",
        8 => "boot data",
        _ => "other",
    };
    let mut changed = vec![];
    for id in ids(&base) {
        let (b, p) = (base.section(id).unwrap(), fw.section(id).unwrap());
        let same = b.stored == p.stored && b.data == p.data && b.dest == p.dest;
        let sizes = format!(
            "{} bytes decoded, {} stored, sha256 {}",
            p.data.len(),
            p.stored.len(),
            &sha256_hex(&p.stored)[..16]
        );
        if PATCHABLE.contains(&id) {
            if same {
                o.ok(format!("section {id} ({}) unchanged: {sizes}", name(id)));
            } else {
                changed.push(id);
                o.ok(format!(
                    "section {id} ({}) differs from the stock base (allowed): {} decoded (stock {}), {} stored (stock {})",
                    name(id),
                    p.data.len(),
                    b.data.len(),
                    p.stored.len(),
                    b.stored.len()
                ));
            }
        } else if same {
            o.ok(format!(
                "section {id} ({}) identical to the stock base, stored and decoded: {sizes}",
                name(id)
            ));
        } else {
            o.fail(format!(
                "section {id} ({}) DIFFERS from the stock base (never allowed): stored {}/{} bytes equal {}, decoded {}/{} equal {}, dest {:#x}/{:#x}",
                name(id),
                p.stored.len(),
                b.stored.len(),
                b.stored == p.stored,
                p.data.len(),
                b.data.len(),
                b.data == p.data,
                p.dest,
                b.dest
            ));
        }
    }
    if changed.is_empty() {
        o.fail("sections 3 and 7 are both identical to the base: nothing is patched");
    } else {
        let s: Vec<String> = changed.iter().map(|i| i.to_string()).collect();
        let only = if changed == [3] {
            " (MAIN OS only; the DSP section is stock)"
        } else if changed == [7] {
            " (DSP only; MAIN OS is stock)"
        } else {
            ""
        };
        o.ok(format!("changed sections: {}{only}", s.join(" and ")));
    }

    // Container checksum, HMAC, section decode, expansion segments, size limits (verify), without a base.
    match verify::verify(patched, Some(reg), None) {
        Ok(rep) => {
            for c in &rep.checks {
                o.ok(c.clone());
            }
            match reg.find(&base.syx_sha256) {
                Some(p) => {
                    let l = &p.limits;
                    let dsp = fw.section(7).unwrap();
                    o.ok(format!(
                        "port {}: container {} of {} bytes, DSP stored {} of {} / decoded {} of {}, MAIN staged {} of {}",
                        p.id(),
                        rep.container_bytes,
                        l.container_max,
                        dsp.stored.len(),
                        l.dsp_stored_max,
                        dsp.data.len(),
                        l.dsp_depacked_max,
                        fw.section(3).map(|m| m.data.len()).unwrap_or(0),
                        l.main_staged_max
                    ));
                }
                None => o.fail(format!(
                    "no port lockfile for the base {} (devices/ports/)",
                    base.syx_sha256
                )),
            }
            if rep.record.as_ref().map(|r| r.base_sha256.as_str()) != Some(base.syx_sha256.as_str())
            {
                o.fail("the record's base differs from the stock file");
            }
        }
        Err(e) => o.fail(format!("verify: {e}")),
    }
    match fw.section(7) {
        Some(dsp) => match crate::dspscan::check_section7(&dsp.data, code) {
            Ok(t) => o.ok(format!("ISA check: {t}")),
            Err(e) => o.fail(format!("ISA check: {e}")),
        },
        None => o.fail("ISA check: the image has no DSP section (7)"),
    }
    o.ok(format!("sha256 patched {}", fw.syx_sha256));
    o.ok(format!("sha256 base    {}", base.syx_sha256));
    o
}
