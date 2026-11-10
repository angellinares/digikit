//! Hash-bound product and firmware identities for deterministic runners.
//!
//! The canonical records live in the repository's `devices/*.toml` files and
//! are embedded at compile time, so native and wasm callers need no filesystem.
//! Native callers can also read them at run time (`Registry::load`), so a new
//! firmware hash needs a TOML edit and no rebuild.
//! A boot contract identifies the existing explicit Oracle MAIN diagnostic
//! path; it is not a hardware-topology description.

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

mod card;
pub use card::{
    CardLayout, PathFsLayout, Pool, PoolLayout, PoolSet, RegionLayout, RegionRole, StoreLayout,
};

/// The profiles embedded at compile time: file name under `devices/` and its text. A new device is one more line.
const EMBEDDED: &[(&str, &str)] = &[
    (
        "digitakt-ii.toml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../devices/digitakt-ii.toml"
        )),
    ),
    (
        "digitone-ii.toml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../devices/digitone-ii.toml"
        )),
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    devices: Vec<DeviceProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    pub name: String,
    pub short: String,
    pub firmwares: Vec<FirmwareProfile>,
    pub panel: Option<PanelProfile>,
    /// Where the card keeps slots, project records, the path filesystem and the module store.
    pub card: Option<CardLayout>,
    /// RAM the emulator keeps across a soft reset (`[digikit] keep_base`, `keep_bytes`): the digikit runtime's crash record. The real
    /// device may or may not keep it **[O]** (docs/findings/46-digikit-debug.md); this is the model.
    pub keep_ram: Option<(u32, u32)>,
    /// Which backend takes samples and modules (`[storage]`), for the Transfer page.
    pub storage: Option<StorageProfile>,
}

/// `[storage]`: the backend of each kind of content. `samples`: "stock-rpc" (the device's MidiRpc file commands) or "digikit" (the digikit store).
/// `modules`: "digikit".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageProfile {
    pub samples: String,
    pub modules: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelProfile {
    pub linear_channels: u8,
    pub encoders: u8,
    pub exceptions: BTreeMap<u8, (u8, u8)>,
}

impl PanelProfile {
    pub fn button(&self, code: u8) -> Option<(u8, u8)> {
        if code == 0 {
            return None;
        }
        let linear = u16::from(self.linear_channels) * 8;
        if u16::from(code) <= linear {
            let value = code - 1;
            Some((value / 8, value % 8))
        } else {
            self.exceptions.get(&code).copied()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareProfile {
    pub version: String,
    pub sha256: String,
    pub filename: Option<String>,
    /// The release's build stamp: the text of section 5 (`meta`) of its ELE3
    /// container, e.g. "260908 14:25:18". A build that changes other sections
    /// keeps it, so it names the release the build was made from
    /// (`boot_for_stamp`).
    pub build_stamp: Option<String>,
    pub boot: Option<BootProfile>,
    pub plusdrive_project_contract: Option<PlusdriveProjectContract>,
    pub readiness_contract: Option<ReadinessContract>,
    pub dsp: Option<DspProfile>,
    /// The softfloat ABI shortcuts (packages/emulator/boot softfloat) are verified for this
    /// firmware's MAIN OS. Off when absent: the emulator then runs the library
    /// code itself.
    pub softfloat_abi: bool,
}

/// The SHARC+ DSP behind the coupled audio of one firmware.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DspProfile {
    /// Image name of `tools/sharc_dev/sharcdb.py` / `tools/sharc_dev/sharc_pack_image.py`.
    pub image: String,
    /// RTOS idle task spin loop, short-word PCs: `[idle_start, idle_end)`.
    pub idle_start: u32,
    pub idle_end: u32,
    pub idle_head: u32,
    /// How card samples reach the DSP, when the ColdFire pushes them.
    #[serde(default)]
    pub sample_link: Option<SampleLink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SampleLink {
    /// ColdFire FlexBus writes to SHARC link port 0 (findings 15).
    Lp0,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootProfile {
    pub main_sha256: String,
    pub contract: BootContract,
    pub symbol_profile: SymbolProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootContract {
    MainOsOracleV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolProfile {
    ElektronRtosV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlusdriveProjectContract {
    #[serde(rename = "dt2-v3-default-1.16")]
    Dt2V3Default116,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadinessContract {
    MainPanelFsCheckV1,
    MainPanelV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    Parse { source: String, detail: String },
    Invalid { source: String, detail: String },
    UnknownFirmware { sha256: String },
    UnsupportedBoot { version: String, sha256: String },
    MainDigestMismatch { expected: String, actual: String },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { source, detail } => write!(f, "cannot parse {source}: {detail}"),
            Self::Invalid { source, detail } => {
                write!(f, "invalid device profile {source}: {detail}")
            }
            Self::UnknownFirmware { sha256 } => {
                write!(f, "no device profile recognizes firmware SHA-256 {sha256}")
            }
            Self::UnsupportedBoot { version, sha256 } => write!(
                f,
                "firmware {version} ({sha256}) has no supported boot contract"
            ),
            Self::MainDigestMismatch { expected, actual } => write!(
                f,
                "MAIN SHA-256 mismatch: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Debug, Deserialize)]
struct SourceDocument {
    device: SourceDevice,
    #[serde(default)]
    firmware: Vec<SourceFirmware>,
    panel: Option<SourcePanel>,
    card: Option<CardLayout>,
    digikit: Option<SourceDigikit>,
    storage: Option<StorageProfile>,
}

#[derive(Debug, Deserialize)]
struct SourceDigikit {
    keep_base: Option<u32>,
    keep_bytes: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SourceDevice {
    name: String,
    short: String,
}

#[derive(Debug, Deserialize)]
struct SourcePanel {
    linear_channels: u8,
    encoders: u8,
    #[serde(default)]
    exceptions: BTreeMap<String, [u8; 2]>,
}

#[derive(Debug, Deserialize)]
struct SourceFirmware {
    version: String,
    sha256: String,
    filename: Option<String>,
    build_stamp: Option<String>,
    main_sha256: Option<String>,
    boot_contract: Option<BootContract>,
    symbol_profile: Option<SymbolProfile>,
    plusdrive_project_contract: Option<PlusdriveProjectContract>,
    readiness_contract: Option<ReadinessContract>,
    dsp: Option<DspProfile>,
    #[serde(default)]
    softfloat_abi: bool,
}

impl Registry {
    /// Parses canonical device documents. Physical-panel fields are purposely
    /// ignored here; callers resolve firmware identity and contracts only.
    pub fn parse(sources: &[(&str, &str)]) -> Result<Self, RegistryError> {
        let mut devices = Vec::with_capacity(sources.len());
        let mut ids = HashSet::new();
        let mut hashes = HashSet::new();
        let mut stamps = HashSet::new();

        for &(source, text) in sources {
            let parsed: SourceDocument =
                toml::from_str(text).map_err(|error| RegistryError::Parse {
                    source: source.into(),
                    detail: error.to_string(),
                })?;
            if parsed.device.name.trim().is_empty() || parsed.device.short.trim().is_empty() {
                return invalid(source, "device name and short ID must be non-empty");
            }
            if !ids.insert(parsed.device.short.clone()) {
                return invalid(
                    source,
                    format!("duplicate device ID {}", parsed.device.short),
                );
            }

            let mut firmwares = Vec::with_capacity(parsed.firmware.len());
            for firmware in parsed.firmware {
                if firmware.version.trim().is_empty() {
                    return invalid(source, "firmware version must be non-empty");
                }
                let sha256 = normalized_hex(source, "firmware SHA-256", firmware.sha256)?;
                if !hashes.insert(sha256.clone()) {
                    return invalid(source, format!("duplicate firmware SHA-256 {sha256}"));
                }
                if let Some(stamp) = &firmware.build_stamp {
                    if stamp.trim().is_empty() || stamp.trim() != stamp {
                        return invalid(source, "build_stamp must be non-empty and trimmed");
                    }
                    if !stamps.insert(stamp.clone()) {
                        return invalid(source, format!("duplicate build_stamp {stamp}"));
                    }
                }
                let boot = match (
                    firmware.main_sha256,
                    firmware.boot_contract,
                    firmware.symbol_profile,
                ) {
                    (None, None, None) => None,
                    (Some(main_sha256), Some(contract), Some(symbol_profile)) => {
                        Some(BootProfile {
                            main_sha256: normalized_hex(source, "MAIN SHA-256", main_sha256)?,
                            contract,
                            symbol_profile,
                        })
                    }
                    _ => {
                        return invalid(
                            source,
                            "main_sha256, boot_contract, and symbol_profile must be supplied together",
                        );
                    }
                };
                firmwares.push(FirmwareProfile {
                    version: firmware.version,
                    sha256,
                    filename: firmware.filename,
                    build_stamp: firmware.build_stamp,
                    boot,
                    plusdrive_project_contract: firmware.plusdrive_project_contract,
                    readiness_contract: firmware.readiness_contract,
                    dsp: firmware.dsp,
                    softfloat_abi: firmware.softfloat_abi,
                });
            }
            let panel = parsed
                .panel
                .map(|panel| validate_panel(source, panel))
                .transpose()?;
            if let Some(card) = &parsed.card {
                card.validate().map_err(|detail| RegistryError::Invalid {
                    source: source.into(),
                    detail: format!("[card] {detail}"),
                })?;
            }
            devices.push(DeviceProfile {
                name: parsed.device.name,
                short: parsed.device.short,
                firmwares,
                panel,
                card: parsed.card,
                keep_ram: parsed
                    .digikit
                    .and_then(|k| Some((k.keep_base?, k.keep_bytes?))),
                storage: parsed.storage,
            });
        }
        Ok(Self { devices })
    }

    /// Loads the canonical repository documents embedded in this crate.
    pub fn embedded() -> Result<Self, RegistryError> {
        let sources: Vec<(String, &str)> = EMBEDDED
            .iter()
            .map(|&(file, text)| (format!("devices/{file}"), text))
            .collect();
        let borrowed: Vec<(&str, &str)> = sources.iter().map(|(n, t)| (n.as_str(), *t)).collect();
        Self::parse(&borrowed)
    }

    /// The registry a runner should use: the documents in `$DIGI_DEVICES_DIR`
    /// (or, failing that, `devices/` beside the working directory or an ancestor
    /// of the executable) when the two device files are there, else the embedded
    /// copy. A document that is present but does not parse is an error, not a
    /// silent fallback. Not available on wasm, which has no filesystem.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load() -> Result<Self, RegistryError> {
        use std::path::PathBuf;
        let files: Vec<&str> = EMBEDDED.iter().map(|&(file, _)| file).collect();
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(dir) = std::env::var_os("DIGI_DEVICES_DIR") {
            dirs.push(dir.into());
        } else {
            if let Ok(cwd) = std::env::current_dir() {
                dirs.extend(cwd.ancestors().map(|a| a.join("devices")));
            }
            if let Ok(exe) = std::env::current_exe() {
                dirs.extend(exe.ancestors().skip(1).map(|a| a.join("devices")));
            }
        }
        for dir in dirs {
            let texts: Vec<_> = files
                .iter()
                .map(|f| std::fs::read_to_string(dir.join(f)))
                .collect();
            if texts.iter().all(Result::is_ok) {
                let names: Vec<String> = files
                    .iter()
                    .map(|f| dir.join(f).display().to_string())
                    .collect();
                let texts: Vec<String> = texts.into_iter().map(Result::unwrap).collect();
                let sources: Vec<(&str, &str)> = names
                    .iter()
                    .map(String::as_str)
                    .zip(texts.iter().map(String::as_str))
                    .collect();
                return Self::parse(&sources);
            }
        }
        Self::embedded()
    }

    #[cfg(target_arch = "wasm32")]
    pub fn load() -> Result<Self, RegistryError> {
        Self::embedded()
    }

    pub fn devices(&self) -> &[DeviceProfile] {
        &self.devices
    }

    /// The card layout of a device named by its short id.
    pub fn card_layout(&self, device: &str) -> Option<&CardLayout> {
        self.devices
            .iter()
            .find(|d| d.short == device)
            .and_then(|d| d.card.as_ref())
    }

    /// The `[storage]` backends of a device named by its short id.
    pub fn storage(&self, device: &str) -> Option<&StorageProfile> {
        self.devices
            .iter()
            .find(|d| d.short == device)
            .and_then(|d| d.storage.as_ref())
    }

    /// The RAM range `(base, bytes)` a soft reset keeps in the emulator's model, for a device named by its short id.
    pub fn keep_ram(&self, device: &str) -> Option<(u32, u32)> {
        self.devices
            .iter()
            .find(|d| d.short == device)
            .and_then(|d| d.keep_ram)
    }

    /// The DSP profile of a firmware named by device short id and version.
    pub fn dsp_for(&self, device: &str, version: &str) -> Option<&DspProfile> {
        self.devices
            .iter()
            .filter(|d| d.short == device)
            .flat_map(|d| d.firmwares.iter())
            .find(|f| f.version == version)
            .and_then(|f| f.dsp.as_ref())
    }

    pub fn identify(
        &self,
        sha256: &str,
    ) -> Result<(&DeviceProfile, &FirmwareProfile), RegistryError> {
        let sha256 = normalized_hex("firmware lookup", "SHA-256", sha256.into())?;
        self.devices
            .iter()
            .flat_map(|device| {
                device
                    .firmwares
                    .iter()
                    .map(move |firmware| (device, firmware))
            })
            .find(|(_, firmware)| firmware.sha256 == sha256)
            .ok_or(RegistryError::UnknownFirmware { sha256 })
    }

    /// The boot profile of a known firmware named by its SHA-256, without
    /// looking at the bytes being booted. For a patched build of that
    /// firmware: the caller vouches that the patch stays within what the
    /// profile's signatures describe (each lookup still has to resolve).
    pub fn boot_for_base(
        &self,
        base_sha256: &str,
    ) -> Result<(&DeviceProfile, &FirmwareProfile, &BootProfile), RegistryError> {
        let (device, firmware) = self.identify(base_sha256)?;
        let Some(boot) = firmware.boot.as_ref() else {
            return Err(RegistryError::UnsupportedBoot {
                version: firmware.version.clone(),
                sha256: firmware.sha256.clone(),
            });
        };
        Ok((device, firmware, boot))
    }

    /// The boot profile of the release whose build stamp is `stamp`, for a
    /// modified build: no profile knows its SHA-256 or its MAIN, but it keeps
    /// its release's section 5 (`meta`). Nothing in the bytes is checked; each
    /// signature lookup still has to resolve, as for `boot_for_base`. `None`
    /// when no release has this stamp.
    pub fn boot_for_stamp(
        &self,
        stamp: &str,
    ) -> Option<Result<(&DeviceProfile, &FirmwareProfile, &BootProfile), RegistryError>> {
        let stamp = stamp.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        let (device, firmware) = self.devices.iter().find_map(|device| {
            device
                .firmwares
                .iter()
                .find(|firmware| firmware.build_stamp.as_deref() == Some(stamp))
                .map(|firmware| (device, firmware))
        })?;
        Some(match firmware.boot.as_ref() {
            Some(boot) => Ok((device, firmware, boot)),
            None => Err(RegistryError::UnsupportedBoot {
                version: firmware.version.clone(),
                sha256: firmware.sha256.clone(),
            }),
        })
    }

    /// The boot profile of the known firmware whose MAIN OS is `main`, for a
    /// .syx whose other sections differ from every known firmware (a changed
    /// DSP stream): the file's own SHA-256 is unknown, its MAIN is not.
    pub fn boot_by_main(
        &self,
        main: &[u8],
    ) -> Option<(&DeviceProfile, &FirmwareProfile, &BootProfile)> {
        let actual = hex_digest(main);
        self.devices.iter().find_map(|device| {
            device.firmwares.iter().find_map(|firmware| {
                let boot = firmware.boot.as_ref()?;
                (boot.main_sha256 == actual).then_some((device, firmware, boot))
            })
        })
    }

    /// Looks up a firmware and verifies its MAIN bytes before returning the
    /// supported explicit Oracle diagnostic contract.
    pub fn boot_for_main(
        &self,
        syx_sha256: &str,
        main: &[u8],
    ) -> Result<(&DeviceProfile, &FirmwareProfile, &BootProfile), RegistryError> {
        let (device, firmware) = self.identify(syx_sha256)?;
        let Some(boot) = firmware.boot.as_ref() else {
            return Err(RegistryError::UnsupportedBoot {
                version: firmware.version.clone(),
                sha256: firmware.sha256.clone(),
            });
        };
        let actual = hex_digest(main);
        if boot.main_sha256 != actual {
            return Err(RegistryError::MainDigestMismatch {
                expected: boot.main_sha256.clone(),
                actual,
            });
        }
        Ok((device, firmware, boot))
    }
}

fn validate_panel(source: &str, panel: SourcePanel) -> Result<PanelProfile, RegistryError> {
    if panel.linear_channels > 16 || panel.encoders > 16 {
        return invalid(
            source,
            "panel linear_channels and encoders must be at most 16",
        );
    }
    let mut coordinates = HashSet::new();
    for channel in 0..panel.linear_channels {
        for bit in 0..8 {
            coordinates.insert((channel, bit));
        }
    }
    let mut exceptions = BTreeMap::new();
    for (code, [channel, bit]) in panel.exceptions {
        let code = code.parse::<u8>().map_err(|_| RegistryError::Invalid {
            source: source.into(),
            detail: "panel exception code must be u8".into(),
        })?;
        if code == 0 || channel >= 16 || bit >= 8 || !coordinates.insert((channel, bit)) {
            return invalid(
                source,
                "panel exception overlaps linear or another exception",
            );
        }
        if exceptions.insert(code, (channel, bit)).is_some() {
            return invalid(source, "duplicate panel exception code");
        }
    }
    Ok(PanelProfile {
        linear_channels: panel.linear_channels,
        encoders: panel.encoders,
        exceptions,
    })
}

fn invalid<T>(source: &str, detail: impl Into<String>) -> Result<T, RegistryError> {
    Err(RegistryError::Invalid {
        source: source.into(),
        detail: detail.into(),
    })
}

fn normalized_hex(source: &str, field: &str, value: String) -> Result<String, RegistryError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return invalid(source, format!("{field} must be 64 hexadecimal characters"));
    }
    Ok(value.to_ascii_lowercase())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in digest {
        use fmt::Write as _;
        write!(&mut text, "{byte:02x}").expect("writing to String cannot fail");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT2: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../devices/digitakt-ii.toml"
    ));
    const DN2: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../devices/digitone-ii.toml"
    ));

    #[test]
    fn embedded_registry_has_the_four_canonical_releases() {
        let registry = Registry::embedded().unwrap();
        let releases: Vec<_> = registry
            .devices()
            .iter()
            .flat_map(|device| {
                device
                    .firmwares
                    .iter()
                    .map(move |firmware| (device.short.as_str(), firmware.version.as_str()))
            })
            .collect();
        assert_eq!(
            releases,
            [
                ("dt2", "1.15C"),
                ("dt2", "1.16"),
                ("dt2", "1.17"),
                ("dn2", "1.10E"),
                ("dn2", "1.11"),
                ("dn2", "1.12")
            ]
        );
    }

    #[test]
    fn softfloat_abi_is_declared_per_firmware() {
        let registry = Registry::embedded().unwrap();
        let on: Vec<_> = registry
            .devices()
            .iter()
            .flat_map(|device| {
                device
                    .firmwares
                    .iter()
                    .filter(|f| f.softfloat_abi)
                    .map(move |f| (device.short.as_str(), f.version.as_str()))
            })
            .collect();
        assert_eq!(
            on,
            [
                ("dt2", "1.16"),
                ("dt2", "1.17"),
                ("dn2", "1.11"),
                ("dn2", "1.12")
            ]
        );
    }

    #[test]
    fn run_time_documents_replace_the_embedded_ones() {
        let dir = std::env::temp_dir().join(format!("digi-devices-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let extra = format!(
            "{DT2}\n[[firmware]]\nversion = \"9.99\"\nsha256 = \"{}\"\n",
            "ab".repeat(32)
        );
        std::fs::write(dir.join("digitakt-ii.toml"), extra).unwrap();
        std::fs::write(dir.join("digitone-ii.toml"), DN2).unwrap();
        // SAFETY: no other test in this crate reads the variable.
        unsafe { std::env::set_var("DIGI_DEVICES_DIR", &dir) };
        let registry = Registry::load().unwrap();
        unsafe { std::env::remove_var("DIGI_DEVICES_DIR") };
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(registry.identify(&"ab".repeat(32)).is_ok());
        assert!(
            Registry::embedded()
                .unwrap()
                .identify(&"ab".repeat(32))
                .is_err()
        );
    }

    #[test]
    fn a_build_stamp_names_one_release() {
        let registry = Registry::embedded().unwrap();
        let (device, firmware, _) = registry.boot_for_stamp("260908 14:25:18").unwrap().unwrap();
        assert_eq!(
            (device.short.as_str(), firmware.version.as_str()),
            ("dn2", "1.11")
        );
        // Section 5 as stored, with padding around the text.
        let (device, firmware, _) = registry
            .boot_for_stamp("260930 10:52:25\0\0")
            .unwrap()
            .unwrap();
        assert_eq!(
            (device.short.as_str(), firmware.version.as_str()),
            ("dt2", "1.17")
        );
        // A known release with no boot contract is refused, not booted.
        assert!(matches!(
            registry.boot_for_stamp("250910 15:18:16"),
            Some(Err(RegistryError::UnsupportedBoot { .. }))
        ));
        assert!(registry.boot_for_stamp("not a stamp").is_none());
        let stamps = registry
            .devices()
            .iter()
            .flat_map(|device| device.firmwares.iter())
            .filter(|firmware| firmware.build_stamp.is_some())
            .count();
        assert_eq!(stamps, 6, "every canonical release has its stamp");
    }

    #[test]
    fn rejects_stamp_ambiguity() {
        let duplicate = DN2.replacen("260908 14:25:18", "260908 14:25:26", 1);
        assert!(
            matches!(Registry::parse(&[("dt2", DT2), ("dn2", &duplicate)]), Err(RegistryError::Invalid { detail, .. }) if detail.contains("duplicate build_stamp"))
        );
    }

    #[test]
    fn rejects_hash_ambiguity() {
        let duplicate = DN2.replacen(
            "2af43e65e3d8390b41c9f66222620f8cce027d73ed87db00c80b440f628472e0",
            "62d588456e47194bd56dfee9568fb9dd4521c4ff1e8b5427eb461355532e8c6c",
            1,
        );
        assert!(
            matches!(Registry::parse(&[("dt2", DT2), ("dn2", &duplicate)]), Err(RegistryError::Invalid { detail, .. }) if detail.contains("duplicate firmware"))
        );
    }

    #[test]
    fn rejects_invalid_boot_schema_and_unknown_contract() {
        let incomplete = DT2.replacen("symbol_profile = \"elektron-rtos-v1\"", "", 1);
        assert!(
            matches!(Registry::parse(&[("dt2", &incomplete)]), Err(RegistryError::Invalid { detail, .. }) if detail.contains("supplied together"))
        );
        let unknown = DT2.replacen("main-os-oracle-v1", "made-up-v1", 1);
        assert!(matches!(
            Registry::parse(&[("dt2", &unknown)]),
            Err(RegistryError::Parse { .. })
        ));
        let unknown_project = DT2.replacen("dt2-v3-default-1.16", "made-up-project", 1);
        assert!(matches!(
            Registry::parse(&[("dt2", &unknown_project)]),
            Err(RegistryError::Parse { .. })
        ));
    }

    #[test]
    fn only_current_dt2_release_has_the_project_contract() {
        let registry = Registry::embedded().unwrap();
        let projects: Vec<_> = registry
            .devices()
            .iter()
            .flat_map(|device| {
                device.firmwares.iter().filter_map(move |firmware| {
                    firmware.plusdrive_project_contract.map(|contract| {
                        (device.short.as_str(), firmware.version.as_str(), contract)
                    })
                })
            })
            .collect();
        assert_eq!(
            projects,
            [
                ("dt2", "1.16", PlusdriveProjectContract::Dt2V3Default116),
                ("dt2", "1.17", PlusdriveProjectContract::Dt2V3Default116)
            ]
        );
    }

    #[test]
    fn boot_requires_the_matching_main_digest() {
        let registry = Registry::embedded().unwrap();
        assert!(matches!(
            registry.boot_for_main(
                "278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec",
                b"wrong MAIN"
            ),
            Err(RegistryError::MainDigestMismatch { .. })
        ));
    }

    #[test]
    fn recognized_old_firmware_has_no_boot_contract() {
        let registry = Registry::embedded().unwrap();
        assert!(matches!(
            registry.boot_for_main(
                "62d588456e47194bd56dfee9568fb9dd4521c4ff1e8b5427eb461355532e8c6c",
                b"anything"
            ),
            Err(RegistryError::UnsupportedBoot { .. })
        ));
    }

    #[test]
    fn panel_profile_maps_exceptions_and_rejects_aliases() {
        let registry = Registry::embedded().unwrap();
        let dt2 = registry
            .devices()
            .iter()
            .find(|device| device.short == "dt2")
            .unwrap();
        let panel = dt2.panel.as_ref().unwrap();
        assert_eq!(panel.button(1), Some((0, 0)));
        assert_eq!(panel.button(49), Some((6, 1)));
        let alias = DT2.replacen("49 = [6, 1]", "49 = [0, 1]", 1);
        assert!(matches!(
            Registry::parse(&[("dt2", &alias)]),
            Err(RegistryError::Invalid { .. })
        ));
    }

    #[test]
    fn dsp_profiles_name_the_idle_loops_and_the_sample_link() {
        let registry = Registry::embedded().unwrap();
        let dt2 = registry.dsp_for("dt2", "1.16").unwrap();
        assert_eq!(
            (dt2.idle_start, dt2.idle_end, dt2.idle_head),
            (0xb8636d, 0xb863df, 0xb863cf)
        );
        assert_eq!(dt2.sample_link, Some(SampleLink::Lp0));
        let dn2 = registry.dsp_for("dn2", "1.11").unwrap();
        assert_eq!(
            (dn2.idle_start, dn2.idle_end, dn2.idle_head),
            (0xb88a49, 0xb88abb, 0xb88aab)
        );
        assert_eq!(dn2.sample_link, None);
        assert!(registry.dsp_for("dn2", "1.10E").is_none());
    }

    // The formulas the firmware uses, as the old per-device enum spelled them (finding 14).
    #[test]
    fn card_layouts_reproduce_the_firmware_slot_formulas() {
        let registry = Registry::embedded().unwrap();
        let dt2 = registry.card_layout("dt2").unwrap();
        let dn2 = registry.card_layout("dn2").unwrap();
        for slot in 0..128u32 {
            let expect = ((u64::from(slot) + 0xb) & 0xff) << 15;
            for layout in [dt2, dn2] {
                assert_eq!(layout.slot_sector(Pool::Projects, slot), expect);
            }
        }
        for slot in 0..2048u32 {
            let s = u64::from(slot);
            assert_eq!(dt2.slot_sector(Pool::Sounds, slot), (s + 0x2400) * 8);
            assert_eq!(dn2.slot_sector(Pool::Sounds, slot), (s + 0x800) * 2);
        }
        for slot in 0..1024u32 {
            let s = u64::from(slot);
            assert_eq!(
                dt2.slot_sector(Pool::Kits, slot),
                (s * 0x10000 + 0x2c0_0000) >> 9
            );
            assert_eq!(dn2.slot_sector(Pool::Kits, slot), 0x2000 + s * 0x40);
        }
        assert_eq!(dt2.slot_sector(Pool::Projects, 127), 0x8a << 15);
        for (layout, sounds, kits) in [(dt2, 8, 0x80), (dn2, 2, 0x40)] {
            assert_eq!(layout.slot_count(Pool::Projects), 128);
            assert_eq!(layout.slot_count(Pool::Sounds), 2048);
            assert_eq!(layout.slot_count(Pool::Kits), 1024);
            assert_eq!(layout.slot_bytes(Pool::Projects), 0x8000 * 512);
            assert_eq!(layout.slot_bytes(Pool::Sounds), sounds * 512);
            assert_eq!(layout.slot_bytes(Pool::Kits), kits * 512);
            assert_eq!(layout.table_sector, 0x800);
            let bitmaps: Vec<_> = Pool::ALL
                .iter()
                .map(|&p| (layout.pool(p).bitmap_offset, layout.pool(p).bitmap_bytes))
                .collect();
            assert_eq!(bitmaps, [(0, 16), (16, 256), (272, 128)]);
            let boots: Vec<_> = layout
                .regions_of(RegionRole::BootProject)
                .map(|r| r.sector)
                .collect();
            assert_eq!(boots, [0x40000, 0x48000]);
            assert_eq!(
                layout
                    .regions_of(RegionRole::WorkingProject)
                    .next()
                    .unwrap()
                    .sector,
                0x50000
            );
            assert!(layout.regions.iter().all(|r| r.sectors == 0x8000));
            let store = layout.store.as_ref().unwrap();
            assert_eq!(store.end_sector, layout.total_sectors);
            assert_eq!(
                store.header_b(),
                store.end_sector - 35,
                "header B, 32 settings sectors, 2 selection sectors"
            );
            assert_eq!(store.settings_a(), store.end_sector - 34);
            assert!(
                store.first_sector > 0x457fff,
                "the store lies above the MmcFs write guard"
            );
        }
        let registry = Registry::embedded().unwrap();
        assert_eq!(registry.storage("dt2").unwrap().samples, "stock-rpc");
        assert_eq!(registry.storage("dn2").unwrap().samples, "digikit");
        assert!(
            ["dt2", "dn2"]
                .iter()
                .all(|d| registry.storage(d).unwrap().modules == "digikit")
        );
        assert_eq!(dt2.path_fs.as_ref().unwrap().superblock_sector, 0x5d8000);
        assert!(dn2.path_fs.is_none());
    }

    #[test]
    fn a_card_layout_with_overlapping_parts_is_rejected() {
        // the store starts inside the path filesystem (it ends at 0x2df0180)
        let bad = DT2.replace("first_sector = 0x2df1000", "first_sector = 0x2df0000");
        let error = Registry::parse(&[("dt2", &bad)]).unwrap_err().to_string();
        assert!(
            error.contains("[card]") && error.contains("overlaps"),
            "{error}"
        );
        // the store runs past the card, or header B is not the sector before the settings record
        let bad = DT2.replace(
            "end_sector = 0x2e1a000\nselection",
            "end_sector = 0x2e1b000\nselection",
        );
        assert!(Registry::parse(&[("dt2", &bad)]).is_err());
        let bad = DT2.replace("header_b_sector = 0x2e19fdd", "header_b_sector = 0x2e19ff0");
        assert!(Registry::parse(&[("dt2", &bad)]).is_err());
        let bad = DT2.replace("bitmap_bytes = 16", "bitmap_bytes = 8");
        assert!(Registry::parse(&[("dt2", &bad)]).is_err());
    }
}
