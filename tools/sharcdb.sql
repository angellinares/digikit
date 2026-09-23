-- Canned queries for tools/sharcdb.py databases (out/sharcdb/<image>.sqlite).
-- Addresses are short words (sw). One image at a time:
--   sqlite3 -header -column out/sharcdb/dt2-1.16.sqlite
-- Substitute the 0x... literals for your own address; SQLite accepts hex
-- integer literals directly, so no printf()/CAST is needed.

-- Callers of a function, across every edge kind (call, jump, cond_jump,
-- indirect) -- not just CALL. This is the fix for the motivating gap: a
-- `JUMP IF SV` into a function is a caller tools/sharcflow.py's call/return
-- scan alone never sees.
SELECT e.kind, printf('%x', e.from_sw) from_sw, f.name from_function, e.cond, e.delayed, e.note
FROM edges e LEFT JOIN functions f ON f.entry_sw = e.from_function
WHERE e.to_sw = 0x1c2b24
ORDER BY e.from_sw;

-- Who jumps into an address at all (jump/cond_jump/fallthrough only, no
-- calls) -- the exact shape of the 0x1c7053 -> 0x1c71ec gap.
SELECT e.kind, printf('%x', e.from_sw) from_sw, e.cond, e.delayed
FROM edges e
WHERE e.to_sw = 0x1c71ec AND e.kind IN ('jump', 'cond_jump', 'fallthrough')
ORDER BY e.from_sw;

-- Functions with no static caller (edges of any kind) -- candidates for a
-- missed indirect entry, a table-driven dispatch target, or a genuine dead
-- routine. An empty result here from THIS query is still not proof of dead
-- code: it only rules out a static edge, not a runtime one (see CLAUDE.md).
SELECT f.name, printf('%x', f.entry_sw) entry, f.n_insns, f.label
FROM functions f
WHERE NOT EXISTS (SELECT 1 FROM edges e WHERE e.to_function = f.entry_sw)
ORDER BY f.n_insns DESC;

-- Writers and readers of an absolute address: static direct-addressing
-- (mem_access.abs_address) plus any indirect site with no resolvable target
-- (reported separately, since a register-relative access can't be matched
-- to one address here).
SELECT sw_hex, direction, space, form FROM (
  SELECT printf('%x', sw) sw_hex, direction, space, form
  FROM mem_access WHERE abs_address = 0x262138
)
ORDER BY sw_hex;

-- Literal-pointer tables pointing into code: any literal whose value lands
-- in a scanned code block's own range (in_code_range), grouped by the
-- function that holds the pointer and its destination register.
SELECT f.name owner, printf('%x', l.sw) sw, printf('%x', l.value) value, l.form, l.dest_reg
FROM literals l LEFT JOIN insn i ON i.sw = l.sw
LEFT JOIN functions f ON f.entry_sw = i.function_sw
WHERE l.in_code_range = 1
ORDER BY l.value;

-- Unresolved indirect sites (to_sw NULL), by owning function -- the sites a
-- static caller graph can never close without a runtime trace or a
-- manifest-seeded register hypothesis (tools/sharc_discover.py).
SELECT f.name owner, printf('%x', e.from_sw) sw, e.note
FROM edges e LEFT JOIN functions f ON f.entry_sw = e.from_function
WHERE e.kind = 'indirect'
ORDER BY owner, e.from_sw;

-- Cross-image function matches by relocation-tolerant hash (needs ATTACH):
--   ATTACH 'out/sharcdb/dn2-1.11.sqlite' AS dn2;
-- Exact-byte matches (identical code, no relocated field at all) first,
-- then reloc-hash-only matches (same shape, different absolute addresses).
SELECT printf('%x', a.entry_sw) dt2_entry, printf('%x', b.entry_sw) dn2_entry,
       a.n_insns, (a.exact_hash = b.exact_hash) exact_match
FROM func_hash a JOIN dn2.func_hash b ON a.reloc_hash = b.reloc_hash
ORDER BY exact_match DESC, a.n_insns DESC;

-- The same, narrowed to one candidate pair (sanity-check a specific match).
SELECT a.entry_sw, b.entry_sw, a.exact_hash = b.exact_hash exact_match, a.reloc_hash = b.reloc_hash reloc_match
FROM func_hash a, dn2.func_hash b
WHERE a.entry_sw = 0x1cbf07 AND b.entry_sw = 0xb806f5;
