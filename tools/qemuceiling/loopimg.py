"""The shared payload for tools/qemuceiling/ceiling.py: a hand-assembled,
self-looping ColdFire ALU loop, plus a from-scratch big-endian ELF32/EM_68K
wrapper so it boots under `qemu-system-m68k -M mcf5208evb -kernel`.

No cross-compiler is used or required -- the loop is two instructions, hand-
encoded, so the same four bytes run unmodified in QEMU and in Unicorn
(UC_CPU_M68K_CFV4E). That is what makes "same instruction count by
construction" true: neither side is asked to count instructions, because the
loop body is fixed and known before either one runs it.

    ADDQ.L #1,D0      0x5280
    BRA.s  <self>     0x60FC   (branch displacement -4: back to the ADDQ)

Four bytes, two instructions, infinite loop. Every trip around it is one D0
increment, so reading D0 back after running for a while gives the iteration
count exactly, and instructions executed = 2 * D0 (verified empirically: see
ceiling.py's docstring for the Unicorn and QEMU numbers this was checked
against).

LOAD_ADDR is 0x2000, not 0x0. Unicorn's m68k support treats PC == 0 as an
end-of-run sentinel (see emu/longrun.py `spin()`'s "pc zero" stop reason for
the same behaviour elsewhere in this project) -- starting a Unicorn run at
0x0 executes nothing and returns immediately, silently, with D0 still 0. This
was caught empirically (a `UC_HOOK_CODE` probe at 0x0 never fired) before it
could produce a false "QEMU is N times faster" number. Any non-zero address
that both sides can execute from works; QEMU's mcf5208evb boots an ELF at
0x2000 as readily as at 0x0, so 0x2000 is used on both sides for one shared
image.
"""
import struct

CODE = bytes([0x52, 0x80, 0x60, 0xFC])   # addq.l #1,d0 ; bra.s <self>
INSTRS_PER_ITER = 2
LOAD_ADDR = 0x2000


def make_elf(code=CODE, entry=LOAD_ADDR):
    """-> a minimal ET_EXEC/EM_68K/big-endian ELF32 with one PT_LOAD segment
    holding `code` at `entry`, entry point `entry`. No section headers, no
    string table, nothing an ELF consumer would need beyond one loadable
    segment and an entry point -- which is all `-kernel`'s ELF loader reads
    for a bare-metal (non-uClinux-bootinfo) image.
    """
    EHSIZE, PHSIZE = 52, 32
    e_ident = b'\x7fELF' + bytes([1, 2, 1, 0]) + b'\x00' * 8   # 32-bit, MSB, v1
    ph_off = EHSIZE
    file_off = EHSIZE + PHSIZE
    eh = e_ident + struct.pack(
        '>HHIIIIIHHHHHH',
        2,          # e_type    ET_EXEC
        4,          # e_machine EM_68K
        1,          # e_version
        entry,      # e_entry
        ph_off,     # e_phoff
        0,          # e_shoff
        0,          # e_flags
        EHSIZE,     # e_ehsize
        PHSIZE,     # e_phentsize
        1,          # e_phnum
        0, 0, 0,    # e_shentsize, e_shnum, e_shstrndx
    )
    ph = struct.pack(
        '>IIIIIIII',
        1,          # p_type PT_LOAD
        file_off,   # p_offset
        entry,      # p_vaddr
        entry,      # p_paddr
        len(code),  # p_filesz
        len(code),  # p_memsz
        5,          # p_flags R+X
        4,          # p_align
    )
    return eh + ph + code


if __name__ == '__main__':
    import sys
    out = sys.argv[1] if len(sys.argv) > 1 else 'loop.elf'
    with open(out, 'wb') as fh:
        fh.write(make_elf())
    print('wrote %s: entry=%#x, %d bytes of code' % (out, LOAD_ADDR, len(CODE)))
