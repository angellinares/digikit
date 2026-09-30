//! Hash-bound product and firmware identities for deterministic runners.
//!
//! The canonical records live in the repository's `devices/*.toml` files and
//! are embedded at compile time, so native and wasm callers need no filesystem.
//! A boot contract identifies the existing explicit Oracle MAIN diagnostic
//! path; it is not a hardware-topology description.

use std::{collections::HashSet, fmt};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const DT2_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../devices/digitakt-ii.toml"
));
const DN2_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../devices/digitone-ii.toml"
));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    devices: Vec<DeviceProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    pub name: String,
    pub short: String,
    pub firmwares: Vec<FirmwareProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareProfile {
    pub version: String,
    pub sha256: String,
    pub filename: Option<String>,
    pub boot: Option<BootProfile>,
    pub plusdrive_project_contract: Option<PlusdriveProjectContract>,
    pub readiness_contract: Option<ReadinessContract>,
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
}

#[derive(Debug, Deserialize)]
struct SourceDevice {
    name: String,
    short: String,
}

#[derive(Debug, Deserialize)]
struct SourceFirmware {
    version: String,
    sha256: String,
    filename: Option<String>,
    main_sha256: Option<String>,
    boot_contract: Option<BootContract>,
    symbol_profile: Option<SymbolProfile>,
    plusdrive_project_contract: Option<PlusdriveProjectContract>,
    readiness_contract: Option<ReadinessContract>,
}

impl Registry {
    /// Parses canonical device documents. Physical-panel fields are purposely
    /// ignored here; callers resolve firmware identity and contracts only.
    pub fn parse(sources: &[(&str, &str)]) -> Result<Self, RegistryError> {
        let mut devices = Vec::with_capacity(sources.len());
        let mut ids = HashSet::new();
        let mut hashes = HashSet::new();

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
                    boot,
                    plusdrive_project_contract: firmware.plusdrive_project_contract,
                    readiness_contract: firmware.readiness_contract,
                });
            }
            devices.push(DeviceProfile {
                name: parsed.device.name,
                short: parsed.device.short,
                firmwares,
            });
        }
        Ok(Self { devices })
    }

    /// Loads the two canonical repository documents embedded in this crate.
    pub fn embedded() -> Result<Self, RegistryError> {
        Self::parse(&[
            ("devices/digitakt-ii.toml", DT2_SOURCE),
            ("devices/digitone-ii.toml", DN2_SOURCE),
        ])
    }

    pub fn devices(&self) -> &[DeviceProfile] {
        &self.devices
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
        "/../../devices/digitakt-ii.toml"
    ));
    const DN2: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../devices/digitone-ii.toml"
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
                ("dn2", "1.10E"),
                ("dn2", "1.11")
            ]
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
            [("dt2", "1.16", PlusdriveProjectContract::Dt2V3Default116)]
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
}
