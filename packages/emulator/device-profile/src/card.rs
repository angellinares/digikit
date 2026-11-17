//! Where a device keeps things on its eMMC card: the `[card]` section of `devices/<device>.toml`.
//!
//! Plain data. The +Drive builder, reader and exporter (packages/emulator/plusdrive) take a
//! [`CardLayout`] and know no device by name, so a new device is a TOML section, not Rust.
//! Every number here is in 512-byte sectors unless the name says bytes.

use serde::Deserialize;

pub const SECTOR_BYTES: u64 = 512;

/// The three `MmcFs` slot pools.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Pool {
    Projects,
    Sounds,
    Kits,
}

impl Pool {
    pub const ALL: [Pool; 3] = [Pool::Projects, Pool::Sounds, Pool::Kits];
    /// The folder name under `.mmcfs/` (and the key in `[card.pools.*]`).
    pub fn dir_name(self) -> &'static str {
        match self {
            Pool::Projects => "projects",
            Pool::Sounds => "sounds",
            Pool::Kits => "kits",
        }
    }
    pub fn from_dir_name(name: &str) -> Option<Pool> {
        Pool::ALL.into_iter().find(|p| p.dir_name() == name)
    }
}

/// One slot pool. Slot `n` starts at sector `base_sector + n * stride_sectors` and holds up to `slot_bytes`.
/// The pool's write-protect bits (a set bit is a padlock, not occupancy) are `bitmap_bytes` bytes at `bitmap_offset` in the table sector.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolLayout {
    pub base_sector: u64,
    pub stride_sectors: u64,
    pub slots: u32,
    pub slot_bytes: usize,
    pub bitmap_offset: usize,
    pub bitmap_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegionRole {
    /// A project record the firmware reads at start (a COKi container); the builder seeds it.
    BootProject,
    /// The working project: rewritten from the boot copy at every start.
    WorkingProject,
}

/// A fixed region outside the pools that the firmware uses for a project.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionLayout {
    pub role: RegionRole,
    pub sector: u64,
    pub sectors: u64,
}

/// The folder (path) filesystem. Absent on a device that has none.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathFsLayout {
    pub superblock_sector: u64,
    /// The first sector after the filesystem (the firmware erases `end_sector - superblock_sector` sectors when it formats): the card's sectors above are
    /// not the path filesystem's (finding 59).
    pub end_sector: u64,
}

/// The module and sample store (docs/design/digikit-store.md, version 3): sectors `[first_sector, end_sector)` that nothing in the
/// firmware writes (finding 59).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreLayout {
    pub first_sector: u64,
    pub end_sector: u64,
    /// The last `selection_sectors` sectors of the range hold the runtime's selection record (two copies of one sector, the
    /// runtime writes them with the firmware's block write; docs/design/digikit-store.md). The store builder keeps its files out
    /// of them: its limit is `end_sector - first_sector - selection_sectors - settings_sectors - 1`.
    #[serde(default = "default_selection_sectors")]
    pub selection_sectors: u64,
    /// The `settings_sectors` sectors before the selection record hold the runtime's settings record (two copies of
    /// `sdk/sdk.toml [settings] sectors`; docs/design/digikit-store.md, "The settings record"). The store builder keeps its files out of them.
    #[serde(default = "default_settings_sectors")]
    pub settings_sectors: u64,
    /// Absolute sector of the second store header copy (version 2, docs/design/digikit-store.md): the sector just before the
    /// settings record. Absent: `end_sector - selection_sectors - settings_sectors - 1`.
    #[serde(default)]
    pub header_b_sector: Option<u64>,
}

impl StoreLayout {
    /// Absolute sector of header copy B.
    pub fn header_b(&self) -> u64 {
        self.header_b_sector
            .unwrap_or(self.end_sector - self.selection_sectors - self.settings_sectors - 1)
    }

    /// Absolute sector of settings record copy A; copy B follows `settings_sectors / 2` sectors later.
    pub fn settings_a(&self) -> u64 {
        self.end_sector - self.selection_sectors - self.settings_sectors
    }

    /// Sectors a store (header A, table, data) may use from `first_sector`: the range without header B, the settings record and the selection record.
    pub fn store_limit(&self) -> u64 {
        self.header_b() - self.first_sector
    }
}

fn default_selection_sectors() -> u64 {
    2
}

fn default_settings_sectors() -> u64 {
    32
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolSet {
    pub projects: PoolLayout,
    pub sounds: PoolLayout,
    pub kits: PoolLayout,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardLayout {
    /// `MmcFs` header sector (magic 0xbeef_bace).
    pub header_sector: u64,
    /// `MmcFs` table sector: the three write-protect bitmaps.
    pub table_sector: u64,
    /// The card's size in sectors (the whitelisted part the firmware accepts): the module and sample store runs to its end.
    pub total_sectors: u64,
    pub pools: PoolSet,
    #[serde(default)]
    pub regions: Vec<RegionLayout>,
    pub path_fs: Option<PathFsLayout>,
    pub store: Option<StoreLayout>,
}

impl CardLayout {
    pub fn pool(&self, pool: Pool) -> &PoolLayout {
        match pool {
            Pool::Projects => &self.pools.projects,
            Pool::Sounds => &self.pools.sounds,
            Pool::Kits => &self.pools.kits,
        }
    }
    pub fn slot_count(&self, pool: Pool) -> u32 {
        self.pool(pool).slots
    }
    /// Bytes between the start of a slot and the next region.
    pub fn slot_bytes(&self, pool: Pool) -> usize {
        self.pool(pool).slot_bytes
    }
    pub fn slot_sector(&self, pool: Pool, slot: u32) -> u64 {
        let p = self.pool(pool);
        p.base_sector + u64::from(slot) * p.stride_sectors
    }
    /// Regions of one role, in file order.
    pub fn regions_of(&self, role: RegionRole) -> impl Iterator<Item = &RegionLayout> {
        self.regions.iter().filter(move |r| r.role == role)
    }
    /// The region that starts at `sector`.
    pub fn region_at(&self, sector: u64) -> Option<&RegionLayout> {
        self.regions.iter().find(|r| r.sector == sector)
    }

    /// Reject a layout whose parts overlap or do not fit their own sector.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let mut spans: Vec<(String, u64, u64)> = vec![
            ("header".into(), self.header_sector, self.header_sector + 1),
            ("table".into(), self.table_sector, self.table_sector + 1),
        ];
        for pool in Pool::ALL {
            let p = self.pool(pool);
            let name = pool.dir_name();
            if p.slots == 0 || p.stride_sectors == 0 {
                return Err(format!("{name}: slots and stride_sectors must be nonzero"));
            }
            if p.slot_bytes == 0
                || !(p.slot_bytes as u64).is_multiple_of(SECTOR_BYTES)
                || p.slot_bytes as u64 > p.stride_sectors * SECTOR_BYTES
            {
                return Err(format!(
                    "{name}: slot_bytes must be a sector multiple within the stride"
                ));
            }
            if p.bitmap_bytes * 8 < p.slots as usize
                || p.bitmap_offset + p.bitmap_bytes > SECTOR_BYTES as usize
            {
                return Err(format!(
                    "{name}: the bitmap must hold one bit per slot inside the table sector"
                ));
            }
            let end = p
                .base_sector
                .checked_add(u64::from(p.slots) * p.stride_sectors)
                .ok_or_else(|| format!("{name}: pool range overflows"))?;
            spans.push((format!("pool {name}"), p.base_sector, end));
        }
        // The three bitmaps must not share bytes.
        let mut bitmaps: Vec<_> = Pool::ALL
            .iter()
            .map(|&p| {
                let l = self.pool(p);
                (l.bitmap_offset, l.bitmap_offset + l.bitmap_bytes)
            })
            .collect();
        bitmaps.sort_unstable();
        if bitmaps.windows(2).any(|w| w[0].1 > w[1].0) {
            return Err("pool bitmaps overlap".into());
        }
        for r in &self.regions {
            if r.sectors == 0 {
                return Err("a region has no sectors".into());
            }
            spans.push((
                format!("region {:#x}", r.sector),
                r.sector,
                r.sector + r.sectors,
            ));
        }
        if let Some(fs) = &self.path_fs
            && (fs.end_sector <= fs.superblock_sector || fs.end_sector > self.total_sectors)
        {
            return Err("the path filesystem range is empty or past the card".into());
        }
        if let Some(store) = &self.store {
            if store.first_sector >= store.end_sector {
                return Err("store range is empty".into());
            }
            if store.end_sector > self.total_sectors {
                return Err("the store reaches past the end of the card".into());
            }
            // header A, header B, the selection record and at least a few sectors of table and data
            if store.header_b() <= store.first_sector + 16
                || store.header_b() + 1 + store.settings_sectors + store.selection_sectors
                    != store.end_sector
            {
                return Err("store header copy B must be the sector before the settings record, which lies before the selection record, with room before it".into());
            }
            spans.push(("store".into(), store.first_sector, store.end_sector));
        }
        if let Some(fs) = &self.path_fs {
            spans.push((
                "path filesystem".into(),
                fs.superblock_sector,
                fs.end_sector,
            ));
        }
        for (i, a) in spans.iter().enumerate() {
            for b in &spans[i + 1..] {
                if a.1 < b.2 && b.1 < a.2 {
                    return Err(format!("{} overlaps {}", a.0, b.0));
                }
            }
        }
        // The firmware's own slots and records lie below the path filesystem (the store is above it, or elsewhere).
        if let Some(fs) = &self.path_fs
            && let Some(over) = spans
                .iter()
                .find(|s| s.0 != "store" && s.0 != "path filesystem" && s.2 > fs.superblock_sector)
        {
            return Err(format!("{} reaches the path filesystem", over.0));
        }
        Ok(())
    }
}
