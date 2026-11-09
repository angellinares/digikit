//! Execute the actual assembled private adapter; no stock firmware bytes or device needed.
use coldfire::{Bus, BusError, Cpu};
use patchkit::build::{cf, toolchain::Env};
use patchkit::link::Linked;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

const IRQSTAT: u32 = 0xfc0c_c030;
const MASK: u32 = 0x0031_0000;
const SP: u32 = 0xf000;
const SITES: [u32; 3] = [0x4012_d5c2, 0x4012_d616, 0x4012_d642];

fn compiled() -> Linked {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let env = Env::new(&root);
    env.m68k("as")
        .expect("storage-fault proof requires the ColdFire assembler");
    let object = cf::cf_object(
        &env,
        &[root.join("modules/core/adapters/storage-fault-proof/src/cf/storage-fault.s")],
        &[],
    )
    .unwrap();
    let externs: BTreeMap<_, _> = ['a', 'b', 'c']
        .into_iter()
        .zip(SITES)
        .map(|(name, site)| (format!("storage_fault_site_{name}"), u64::from(site)))
        .collect();
    let linked = patchkit::link::link(&object, 0x1000, &externs).unwrap();
    assert_eq!(
        linked.bss_size, 4,
        "only the boot-scoped sticky word is allocated"
    );
    linked
}

struct Memory {
    bytes: Vec<u8>,
    irq: VecDeque<u32>,
    reads: usize,
    writes: Vec<(u32, usize)>,
}
impl Memory {
    fn new(linked: &Linked, values: &[u32]) -> Self {
        let mut bytes = vec![0xa5; 0x10000];
        bytes[0x1000..0x1000 + linked.image.len()].copy_from_slice(&linked.image);
        let state = linked.symbols["storage_fault_bits"] as usize;
        // Model the existing segment loader's BSS initialization, not a hook clear.
        bytes[state..state + 4].fill(0);
        Self {
            bytes,
            irq: values.iter().copied().collect(),
            reads: 0,
            writes: vec![],
        }
    }
    fn get(&self, at: u32, n: usize) -> Result<&[u8], BusError> {
        self.bytes
            .get(at as usize..at as usize + n)
            .ok_or(BusError {
                addr: at,
                write: false,
            })
    }
    fn put(&mut self, at: u32, bytes: &[u8]) -> Result<(), BusError> {
        self.writes.push((at, bytes.len()));
        self.bytes
            .get_mut(at as usize..at as usize + bytes.len())
            .ok_or(BusError {
                addr: at,
                write: true,
            })?
            .copy_from_slice(bytes);
        Ok(())
    }
    fn word(&self, at: u32) -> u32 {
        u32::from_be_bytes(self.get(at, 4).unwrap().try_into().unwrap())
    }
}
impl Bus for Memory {
    fn read8(&mut self, at: u32) -> Result<u8, BusError> {
        Ok(self.get(at, 1)?[0])
    }
    fn read16(&mut self, at: u32) -> Result<u16, BusError> {
        Ok(u16::from_be_bytes(self.get(at, 2)?.try_into().unwrap()))
    }
    fn read32(&mut self, at: u32) -> Result<u32, BusError> {
        if at == IRQSTAT {
            self.reads += 1;
            return Ok(self
                .irq
                .pop_front()
                .expect("each hook must read IRQSTAT only once"));
        }
        Ok(u32::from_be_bytes(self.get(at, 4)?.try_into().unwrap()))
    }
    fn write8(&mut self, at: u32, value: u8) -> Result<(), BusError> {
        self.put(at, &[value])
    }
    fn write16(&mut self, at: u32, value: u16) -> Result<(), BusError> {
        self.put(at, &value.to_be_bytes())
    }
    fn write32(&mut self, at: u32, value: u32) -> Result<(), BusError> {
        self.put(at, &value.to_be_bytes())
    }
}

fn context(linked: &Linked, hook: char, sr: u16, sp: u32) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.pc = linked.symbols[&format!("storage_fault_hook_{hook}")] as u32;
    for i in 0..8 {
        cpu.d[i] = 0x8765_0010 + i as u32 * 0x101;
    }
    for i in 0..7 {
        cpu.a[i] = 0x1234_1000 + i as u32 * 0x101;
    }
    cpu.a[7] = sp;
    cpu.sr = sr;
    cpu
}
fn finish(cpu: &mut Cpu, ram: &mut Memory, continuation: u32) -> usize {
    for steps in 0..32 {
        if cpu.pc == continuation {
            return steps;
        }
        cpu.step(ram).unwrap();
        assert_eq!(
            cpu.last_exception, None,
            "unexpected exception at {:#x}",
            cpu.pc
        );
    }
    panic!(
        "hook failed to reach continuation {continuation:#x}, at {:#x}",
        cpu.pc
    );
}

#[test]
fn all_three_compiled_hooks_preserve_original_move_and_context() {
    let linked = compiled();
    let state = linked.symbols["storage_fault_bits"] as u32;
    let values = [
        0,
        1,
        0x10000,
        0x100000,
        0x200000,
        0x310001,
        0x80000000,
        u32::MAX,
    ];
    for (hook, site) in ['a', 'b', 'c'].into_iter().zip(SITES) {
        for value in values {
            for control in [0x2000, 0x2300, 0x2700, 0xa200] {
                for ccr in 0..32u16 {
                    let sr = control | ccr;
                    let mut ram = Memory::new(&linked, &[value]);
                    let original = ram.bytes.clone();
                    let mut cpu = context(&linked, hook, sr, SP);
                    let before_d = cpu.d;
                    let before_a = cpu.a;
                    assert_eq!(finish(&mut cpu, &mut ram, site + 6), 13);
                    assert_eq!(cpu.d[0], value);
                    assert_eq!(&cpu.d[1..], &before_d[1..]);
                    assert_eq!(cpu.a, before_a);
                    let nz = if value == 0 {
                        4
                    } else if value & 0x8000_0000 != 0 {
                        8
                    } else {
                        0
                    };
                    assert_eq!(
                        cpu.sr,
                        (sr & !0x0f) | nz,
                        "MOVE N/Z, V/C clear, X/control unchanged"
                    );
                    assert_eq!(ram.reads, 1);
                    assert_eq!(ram.word(state), value & MASK);
                    assert_eq!(ram.writes, [(SP - 8, 4), (SP - 4, 2), (state, 4)]);
                    for (at, (old, new)) in original.iter().zip(&ram.bytes).enumerate() {
                        let allowed = (SP - 8..SP - 2).contains(&(at as u32))
                            || (state..state + 4).contains(&(at as u32));
                        assert!(allowed || old == new, "unexpected write at {at:#x}");
                    }
                    assert_eq!(
                        &ram.bytes[SP as usize..SP as usize + 16],
                        &original[SP as usize..SP as usize + 16],
                        "existing exception frame untouched"
                    );
                }
            }
        }
    }
}

#[test]
fn sticky_bits_survive_zero_repeated_and_error_plus_complete_samples() {
    let linked = compiled();
    let state = linked.symbols["storage_fault_bits"] as u32;
    let values = [0, 0x10001, 0, 0x100000, 0x100001, 0x200001, u32::MAX, 0];
    let mut ram = Memory::new(&linked, &values);
    let mut expected = 0;
    for (i, value) in values.into_iter().enumerate() {
        let hook = ['a', 'b', 'c'][i % 3];
        let mut cpu = context(&linked, hook, 0x231f, SP);
        finish(&mut cpu, &mut ram, SITES[i % 3] + 6);
        expected |= value & MASK;
        assert_eq!(ram.word(state), expected);
    }
    assert_eq!(ram.reads, values.len());
    assert_eq!(expected, MASK);
}

#[test]
fn observer_interleaving_at_each_instruction_boundary_cannot_lose_bits() {
    // Two separately saved interrupt contexts model only inter-instruction
    // observer nesting, not controller delivery or whole-ISR quiescence.
    let linked = compiled();
    let state = linked.symbols["storage_fault_bits"] as u32;
    for boundary in 0..=13 {
        let samples = if boundary == 0 {
            [0x200000, 0x10000]
        } else {
            [0x10000, 0x200000]
        };
        let mut ram = Memory::new(&linked, &samples);
        let mut outer = context(&linked, 'a', 0x2115, SP);
        for _ in 0..boundary {
            outer.step(&mut ram).unwrap();
        }
        let mut inner = context(&linked, 'b', 0x221a, outer.a[7] - 16);
        finish(&mut inner, &mut ram, SITES[1] + 6);
        finish(&mut outer, &mut ram, SITES[0] + 6);
        assert_eq!(ram.word(state), 0x210000, "boundary {boundary}");
        assert_eq!(ram.reads, 2);
    }
}
