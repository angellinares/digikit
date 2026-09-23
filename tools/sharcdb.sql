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

-- --- basic blocks / succ (control flow) --------------------------------
--
-- bblocks/succ are scoped to one loader code block at a time (see
-- tools/sharcdb.py's _build_blocks_and_succ docstring): a branch whose
-- target this pass never disassembled gets a succ row whose to_block
-- matches no bblocks row, same shape as edges.to_function's NULL.

-- Basic block containing a given sw (every other query below builds on
-- this pattern -- substitute the 0x... literal).
SELECT start_sw, end_sw, function_sw, n_insns FROM bblocks
WHERE start_sw <= 0x1c7053 AND end_sw > 0x1c7053;

-- Reachability, sw X to sw Y, over succ alone (stays inside whatever
-- function/loader-code-block succ edges connect -- a call site only
-- contributes its call_return edge back into the caller, never a step into
-- the callee; see "across calls" below for that).
WITH RECURSIVE reach(sw) AS (
  SELECT start_sw FROM bblocks WHERE start_sw <= 0x1c642a AND end_sw > 0x1c642a
  UNION
  SELECT s.to_block FROM reach r JOIN succ s ON s.from_block = r.sw
  WHERE s.to_block IS NOT NULL
)
SELECT EXISTS (
  SELECT 1 FROM reach r JOIN bblocks b ON b.start_sw = r.sw
  WHERE b.start_sw <= 0x1c7053 AND b.end_sw > 0x1c7053
) AS reaches;

-- Reachability across calls: the same walk, but whenever the current block
-- holds a call site (edges.kind='call'), also step into the callee's entry
-- block. This is a may-reach superset (it does not model the call actually
-- returning), meant for "is Y ever downstream of X at all", not a proof of
-- an executable path.
WITH RECURSIVE reach(sw) AS (
  SELECT start_sw FROM bblocks WHERE start_sw <= 0x1c642a AND end_sw > 0x1c642a
  UNION
  SELECT s.to_block FROM reach r JOIN succ s ON s.from_block = r.sw
  WHERE s.to_block IS NOT NULL
  UNION
  SELECT bb.start_sw FROM reach r
    JOIN bblocks b2 ON b2.start_sw = r.sw
    JOIN edges e ON e.from_sw BETWEEN b2.start_sw AND b2.end_sw - 1
      AND e.kind = 'call' AND e.to_sw IS NOT NULL
    JOIN bblocks bb ON bb.start_sw = e.to_sw
)
SELECT EXISTS (
  SELECT 1 FROM reach r JOIN bblocks b ON b.start_sw = r.sw
  WHERE b.start_sw <= 0x1c7053 AND b.end_sw > 0x1c7053
) AS reaches;

-- --- register def/use ---------------------------------------------------

-- Last writer(s) of REG before sw S: walk basic blocks backwards over succ
-- (reversing to_block -> from_block), stopping expansion at the first block
-- on each path that already has a regdef of REG before the search's upper
-- bound there (the target sw itself for the starting block, that block's
-- own end otherwise). Substitute REG ('R6') and S (0x1c6553) three times
-- each.
WITH RECURSIVE walk(block_sw, upper_sw) AS (
  SELECT b0.start_sw, 0x1c6553
  FROM bblocks b0 WHERE b0.start_sw <= 0x1c6553 AND b0.end_sw > 0x1c6553
  UNION
  SELECT s.from_block, b.end_sw
  FROM walk w
  JOIN bblocks b ON b.start_sw = w.block_sw
  JOIN succ s ON s.to_block = w.block_sw
  WHERE NOT EXISTS (
    SELECT 1 FROM regdef d
    WHERE d.reg = 'R6' AND d.sw >= b.start_sw AND d.sw < w.upper_sw
  )
)
SELECT DISTINCT writer_sw FROM (
  SELECT MAX(d.sw) AS writer_sw
  FROM walk w
  JOIN bblocks b ON b.start_sw = w.block_sw
  JOIN regdef d ON d.reg = 'R6' AND d.sw >= b.start_sw AND d.sw < w.upper_sw
  GROUP BY w.block_sw, w.upper_sw
);

-- --- data references -----------------------------------------------------

-- Every site that materialises a given address/constant, with the register
-- it feeds (i_reg_base) or the direction it's read/written in
-- (abs_load/abs_store), or the same for the SAME base+offset resolved from
-- a nearby literal I-register load (resolved_offset).
SELECT printf('%x', sw) sw, form, role FROM dataref WHERE value = 0x8055c840 ORDER BY sw;
