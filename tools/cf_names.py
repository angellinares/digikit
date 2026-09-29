"""Named ColdFire addresses for Digitakt II OS 1.16 (MAIN OS only).

A small, hand-curated table -- not a general cross-firmware resolver like
emu/symbols.py (which derives addresses from signatures so they survive a
firmware rebuild). These are literal 1.16 addresses, good for exactly this
build, collected while reverse-engineering track state, sample loading, the
FlexBus link to the SHARC, and the boot-time UI sequence. Putting them in
emu/symbols.py's Fixed()/Sig() machinery would require reference bytes and a
second firmware to verify against, which this table doesn't have -- so they
live here instead, read by tools/snapread.py (to label a raw address) and
tools/cfdb.py/tools/cf.py (to look up a function by name).

Each entry: NAME -> (address, mark, description, source). `mark` follows
docs/findings/FINDINGS.md's convention (see CLAUDE.md's Rules): '[V]'
verified against the image bytes, '[D]' documented/read once but not
re-checked, '[O]' open, '[C]' corrects an earlier claim, or 'unmarked' where
the source note gave no explicit mark for that specific claim. `source` is
the scratchpad report an address was pulled from -- cf-track-state.md,
sharc-trig-arm.md, drive3-load.md, sample-load-routes.md or display-start.md
-- not yet a docs/findings/ entry; treat these as [D] at best until a second
agent checks them against the image bytes and promotes them there (CLAUDE.md
Rules: "Have a second agent check a finding against the image bytes before
marking it [V]").

    import cf_names
    cf_names.NAME_BY_ADDR[0x400caf48]       # -> "machine_descriptor_dispatch"
    cf_names.ADDRS["machine_descriptor_dispatch"]  # -> (0x400caf48, ...)
"""

from __future__ import annotations

# name -> (address, mark, description, source)
ADDRS: dict[str, tuple[int, str, str, str]] = {
    # --- cf-track-state.md: live kit/track records, SPI2 frame, smoother ---
    "live_track_base_ptr": (
        0x80004704,
        "[V]",
        "_DAT_80004704: pointer to the live 16-track record array (kit data)",
        "cf-track-state.md",
    ),
    "track_sync_cache": (
        0x8000470C,
        "[V]",
        "16-entry DSP-side sync cache, zeroed on track-base change",
        "cf-track-state.md",
    ),
    "live_track_base_setter": (
        0x4002DA7A,
        "[V]",
        "writes live_track_base_ptr and zeroes track_sync_cache",
        "cf-track-state.md",
    ),
    "machine_descriptor_dispatch": (
        0x400CAF48,
        "[V]",
        "machine-type descriptor dispatch (bound moveq #6, fallback entry 6); docs/findings/02",
        "cf-track-state.md",
    ),
    "machine_display_name_table": (
        0x401FBC50,
        "[V]",
        "UI display-name table for machine types; docs/findings/02",
        "cf-track-state.md",
    ),
    "machine_permission_check": (
        0x400DCAB8,
        "[V]",
        "per-machine-type permission check; docs/findings/02",
        "cf-track-state.md",
    ),
    "active_kit_getter": (
        0x40041D24,
        "[D]",
        "returns Project + 0xf4, the active kit object",
        "cf-track-state.md",
    ),
    "track_records_builder": (
        0x4004E598,
        "[D]",
        "builds/returns the 16 track records (0x450 stride each) from the kit",
        "cf-track-state.md",
    ),
    "track_row_copy": (
        0x4002D438,
        "[V]",
        "copies Sound+0xa2 (0x9a bytes) into the DSP-side row and Sound+0x14 into the raw mirror",
        "cf-track-state.md",
    ),
    "track_row_base": (
        0x80003CD0,
        "[V]",
        "DSP-side per-track row mirror, stride 0x9a; byte 0 = machine type",
        "cf-track-state.md",
    ),
    "track_raw_mirror_base": (
        0x80003362,
        "[D]",
        "raw per-track mirror, stride 0x8e; index 28 = sample slot",
        "cf-track-state.md",
    ),
    "spi2_tx_frame_base": (
        0x80005348,
        "[D]",
        "SPI2 TX frame buffer sent to the SHARC",
        "cf-track-state.md",
    ),
    "vector_191_handler": (
        0x4002DD0C,
        "[D]",
        'per-frame SPI2 TX frame builder ("vector_191_handler")',
        "cf-track-state.md",
    ),
    "track_smoothed_mirror_base": (
        0x80005B50,
        "[D]",
        "smoothed per-track parameter mirror, stride 0x8e (A5 in builder)",
        "cf-track-state.md",
    ),
    "param_smoother": (
        0x400D92A2,
        "[D]",
        "one-pole EMAC-fractional smoother for mirrored parameters",
        "cf-track-state.md",
    ),
    "smoother_state_base": (
        0x8000DD40,
        "[D]",
        "smoother filter state buffer",
        "cf-track-state.md",
    ),
    "track_trig_record_base": (
        0x47DB41D0,
        "[D]",
        "per-track 0x14-byte trig-related record array",
        "cf-track-state.md",
    ),
    "boot_project_record_read": (
        0x400F0628,
        "[D]",
        "reads project record at eMMC sector 0x40000/0x48000, validates COKi header",
        "cf-track-state.md",
    ),
    "project_container_decode": (
        0x400C0C2C,
        "[D]",
        "decodes/upgrades project container v3 to v5",
        "cf-track-state.md",
    ),
    "write_live_project": (
        0x400DF7F6,
        "[D]",
        "writes the decoded container into the live Project object",
        "cf-track-state.md",
    ),
    # --- drive3-load.md: +Drive mount, loader slots ---
    "sample_loader_load_sample": (
        0x40154540,
        "[D]",
        "loads one sample by ref into sample RAM (Ghidra mislabels as OnScopeExit::ctor_dtor)",
        "drive3-load.md",
    ),
    "slot_header_mono_builder": (
        0x40153B90,
        "[D]",
        "builds/sends mono (and reset-placeholder) slot header block",
        "drive3-load.md",
    ),
    "slot_header_stereo_builder": (
        0x40153B28,
        "[D]",
        "builds/sends stereo slot header block",
        "drive3-load.md",
    ),
    "loader_slot_reset_all": (
        0x40154358,
        "unmarked",
        "resets all 0x400 loader slots (global reset pass)",
        "drive3-load.md",
    ),
    # --- sample-load-routes.md: sample load path, FlexBus, BgWorker jobs ---
    "main_os_task": (
        0x400337BA,
        "[V]",
        "Main OS task entry, every boot branch",
        "sample-load-routes.md",
    ),
    "queue_load_all_samples": (
        0x400311D0,
        "[V]",
        '"Load all samples" job queued on SampleLoaderBgWorker',
        "sample-load-routes.md",
    ),
    "reload_all_samples_invoker": (
        0x40030CE8,
        "[V]",
        "invoker (not a Ghidra function) that checks the Project singleton and calls reload",
        "sample-load-routes.md",
    ),
    "project_settings_reload_all_samples": (
        0x4004E8BE,
        "[V]",
        "ProjectSettings::reloadAllSamples(bool)",
        "sample-load-routes.md",
    ),
    "project_singleton": (
        0x44F370A0,
        "unmarked",
        "Project singleton pointer",
        "sample-load-routes.md",
    ),
    "sample_ref_resolve": (
        0x4015B178,
        "[D]",
        "resolves a 16-byte sample reference to a record",
        "sample-load-routes.md",
    ),
    "sample_ref_key_check": (
        0x4015AB5C,
        "[V]",
        "canonical key check; requires bit 0 of record +0x0c set",
        "sample-load-routes.md",
    ),
    "sample_page_send": (
        0x40153734,
        "[D]",
        "reads/sends one sample chunk via flexbus_page_send",
        "sample-load-routes.md",
    ),
    "loaded_slot_key_table": (
        0x405B1368,
        "[D]",
        "per-slot loaded-content key table (dedup)",
        "sample-load-routes.md",
    ),
    "loaded_block_dedup_scan": (
        0x401535C2,
        "[D]",
        "scans the loaded-block vector for a matching key (aliasing)",
        "sample-load-routes.md",
    ),
    "sample_event_post": (
        0x40153622,
        "[D]",
        "posts a sample-loaded event to sample_event_queue",
        "sample-load-routes.md",
    ),
    "sample_event_queue": (
        0x402B6628,
        "[D]",
        "ColdFire-side consumer queue for sample-load events",
        "sample-load-routes.md",
    ),
    "flexbus_page_send": (
        0x400CD638,
        "[V]",
        "sends one 4-byte-tag + 4096-byte page over the FlexBus bit-bang link to the SHARC",
        "sample-load-routes.md",
    ),
    "flexbus_word_send_le": (
        0x400CCDA0,
        "[V]",
        "sends one 32-bit word LSB-first over FlexBus bit-bang",
        "sample-load-routes.md",
    ),
    "flexbus_word_send_be16": (
        0x400CCE2C,
        "[V]",
        "sends one 32-bit word as two big-endian int16 halves over FlexBus",
        "sample-load-routes.md",
    ),
    "flexbus_lock": (
        0x42948B14,
        "[V]",
        "mutex guarding the FlexBus transmit routine",
        "sample-load-routes.md",
    ),
    "dtim1_sleep_100us": (
        0x40136268,
        "[D]",
        "100us DTIM1 sleep between FlexBus sends",
        "sample-load-routes.md",
    ),
    "drive_mount": (
        0x4015A450,
        "[D]",
        "+Drive filesystem mount routine",
        "sample-load-routes.md",
    ),
    "fs_mounted_flag": (
        0x44F2BD68,
        "[D]",
        "_DAT_44f2bd68, set nonzero once +Drive is mounted",
        "sample-load-routes.md",
    ),
    "builtin_project_container_a": (
        0x4025F1D8,
        "[V]",
        "built-in factory-reset project container (depacked)",
        "sample-load-routes.md",
    ),
    "builtin_project_container_b": (
        0x4027989C,
        "[V]",
        "built-in #PLAY_PATTERN project container (byte-identical to a)",
        "sample-load-routes.md",
    ),
    "boot_flags_word": (
        0x4029E9B0,
        "[V]",
        "boot flags: bits 0-3 project-init flags, bit 5 = console enable",
        "sample-load-routes.md",
    ),
    "console_task_entry": (
        0x400CAE8C,
        "[V]",
        "1.16 serial console task entry (was a different address on 1.15C)",
        "sample-load-routes.md",
    ),
    "sample_id_bitmap": (
        0x46F58ED0,
        "[D]",
        "RAM id-validity bitmap for sample ids 2..0x57fff",
        "sample-load-routes.md",
    ),
    "sample_hash_index": (
        0x4067D8C0,
        "[D]",
        "{count, pairs{hash,id}} binary-search hash index for id-0 lookups",
        "sample-load-routes.md",
    ),
    "bgworker_ctor": (
        0x400F0C18,
        "[V]",
        "BgWorker::ctor",
        "sample-load-routes.md",
    ),
    "bgworkerbase_ctor": (
        0x400F0774,
        "[V]",
        "BgWorkerBase::ctor (generic worker-thread constructor)",
        "sample-load-routes.md",
    ),
    "bgworker_job_loop": (
        0x400F0958,
        "[V]",
        "shared job-loop body for both BgWorker and SampleLoaderBgWorker",
        "sample-load-routes.md",
    ),
    "bgworker_queue": (
        0x400F0AB8,
        "[D]",
        "BgWorker::queue(...), pushes a job and signals the counting semaphore",
        "sample-load-routes.md",
    ),
    "bgworker_tcb": (
        0x43172DE4,
        "[V]",
        "BgWorker (prio 2) task control block",
        "sample-load-routes.md",
    ),
    "bgworker_task_entry": (
        0x400F0C8E,
        "[V]",
        "BgWorker task entry point",
        "sample-load-routes.md",
    ),
    "sampleloaderbgworker_tcb": (
        0x43176E38,
        "[D]",
        "SampleLoaderBgWorker (prio 3) task control block",
        "sample-load-routes.md",
    ),
    "sampleloaderbgworker_task_entry": (
        0x400F0DA6,
        "[D]",
        "SampleLoaderBgWorker task entry point",
        "sample-load-routes.md",
    ),
    "bgworker_singleton_ptr": (
        0x44F37090,
        "[V]",
        "*0x44f37090, BgWorker singleton object pointer",
        "sample-load-routes.md",
    ),
    # --- display-start.md: boot-time UI sequence (intro/progress/panel) ---
    "vector_table_init": (
        0x40001992,
        "[V]",
        "fills all 0x100 interrupt vector slots with the default handler",
        "display-start.md",
    ),
    "vector_208_slot": (
        0x40000340,
        "[V]",
        "interrupt vector 208's slot in the VBR table",
        "display-start.md",
    ),
    "progress_screen_start": (
        0x40133586,
        "[C]",
        'starts the "INITIALIZING +DRIVE..." progress screen (not the display start)',
        "display-start.md",
    ),
    "progress_task_entry": (
        0x40133646,
        "[V]",
        "progress-screen task entry (prio 6)",
        "display-start.md",
    ),
    "progress_task_tcb": (
        0x44E46564,
        "[V]",
        "progress-screen task TCB",
        "display-start.md",
    ),
    "progress_pit3_isr": (
        0x40133518,
        "[V]",
        "vector 208 ISR during the progress screen; acks PIT3, gives progress_frame_sem",
        "display-start.md",
    ),
    "progress_frame_sem": (
        0x44E460D8,
        "[V]",
        "per-frame semaphore for the progress-screen task",
        "display-start.md",
    ),
    "progress_screen_teardown": (
        0x40133626,
        "[V]",
        "tears down PIT3/vector 208, posts progress_done_sem",
        "display-start.md",
    ),
    "progress_done_sem": (
        0x44E460E0,
        "[V]",
        "signalled when the progress screen finishes (factory reset / migrate)",
        "display-start.md",
    ),
    "intro_isr_installer": (
        0x400D12E4,
        "[V]",
        "creates the intro task and installs the intro's vector-208 ISR",
        "display-start.md",
    ),
    "intro_task_entry": (
        0x400D18AE,
        "[V]",
        "intro animation task entry (prio 7)",
        "display-start.md",
    ),
    "intro_pit3_isr": (
        0x400D0668,
        "[V]",
        "vector 208 ISR during the intro; acks PIT3, gives intro_frame_sem",
        "display-start.md",
    ),
    "intro_frame_sem": (
        0x43149548,
        "[V]",
        "per-frame semaphore for the intro task",
        "display-start.md",
    ),
    "intro_done_sem": (
        0x43149550,
        "[V]",
        "posted when the intro animation finishes; Main OS blocks on this",
        "display-start.md",
    ),
    "intro_teardown": (
        0x400D1934,
        "[V]",
        "intro exit: PIT3 off, sem_post(intro_done_sem)",
        "display-start.md",
    ),
    "panel_flush": (
        0x4013390E,
        "[V]",
        "panel_diff: double-buffer diff + flush to the panel bus (all UI paths)",
        "display-start.md",
    ),
    "fb_front_ptr": (
        0x402B6058,
        "[V]",
        "front framebuffer pointer (DAT_402b6058)",
        "display-start.md",
    ),
    "fb_back_ptr": (
        0x402B605C,
        "[V]",
        "back framebuffer pointer (DAT_402b605c)",
        "display-start.md",
    ),
    "main_ui_render": (
        0x40032992,
        "[V]",
        "main UI render entry (branch C's display path, no PIT3 involved)",
        "display-start.md",
    ),
    "mmc_caches_job_functor": (
        0x40032EAA,
        "[V]",
        '"Update MMC Caches" job\'s std::function manager (no completion callback)',
        "display-start.md",
    ),
    "mmc_caches_scan": (
        0x4012FAEA,
        "[V]",
        '"Update MMC Caches" job body: 3 scans under the MmcFs lock',
        "display-start.md",
    ),
    "mmcfs_lock_acquire": (
        0x4019E0EE,
        "[V]",
        "acquires the MmcFs recursive lock",
        "display-start.md",
    ),
    "esdhc_read_routine": (
        0x4012DEDA,
        "[V]",
        "per-block eSDHC CMD18 read (card, eDMA channel 59)",
        "display-start.md",
    ),
    "esdhc_lock": (
        0x47DE3CAC,
        "[V]",
        "mutex guarding the eSDHC read routine",
        "display-start.md",
    ),
    "esdhc_cmd_done_sem": (
        0x44E3FEC8,
        "[V]",
        "eSDHC command-done semaphore",
        "display-start.md",
    ),
    "esdhc_data_done_sem": (
        0x44E3FEB8,
        "[V]",
        "eSDHC data-done semaphore",
        "display-start.md",
    ),
    "esdhc_dma_done_sem": (
        0x44E3FEC0,
        "[V]",
        "eSDHC DMA-done semaphore",
        "display-start.md",
    ),
    "esdhc_capacity_bound": (
        0x44E3FEA0,
        "[D]",
        "_DAT_44e3fea0, the +Drive/card capacity bound checked before a read",
        "display-start.md",
    ),
}

# Reverse index for a fast address -> name lookup (snapread's use case). Two
# names can never collide on the same address here since ADDRS is keyed by
# name; if a future entry reuses an address under a second name, the later
# dict.items() wins arbitrarily -- fine for a label, not for identity.
NAME_BY_ADDR: dict[int, str] = {
    addr: name for name, (addr, _mark, _desc, _src) in ADDRS.items()
}


def describe(addr: int) -> str | None:
    """'name (description) [mark, source]' for a known address, else None."""
    name = NAME_BY_ADDR.get(addr)
    if name is None:
        return None
    _addr, mark, desc, src = ADDRS[name]
    return "%s (%s) [%s, %s]" % (name, desc, mark, src)


if __name__ == "__main__":
    import sys

    if len(sys.argv) > 1:
        for arg in sys.argv[1:]:
            a = int(arg, 16)
            d = describe(a)
            print("0x%08x  %s" % (a, d or "(unnamed)"))
    else:
        for name, (addr, mark, desc, src) in sorted(
            ADDRS.items(), key=lambda kv: kv[1][0]
        ):
            print("0x%08x  %-36s %-4s %-22s %s" % (addr, name, mark, src, desc))
