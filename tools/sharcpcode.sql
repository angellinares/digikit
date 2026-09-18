-- Queries for the dumps tools/sharcpcode.py writes (OUT/<image>.sqlite).
-- Addresses are short words; decoder.length is bytes. One image at a time:
--   sqlite3 -header -column out/sharcpcode/new/dt2-1.16.sqlite
-- The two-run queries need the other run attached first:
--   ATTACH 'out/sharcpcode/old/dt2-1.16.sqlite' AS old;

-- Each form: how many aligned instructions our decoder finds, how many the
-- language fails to decode, where the two lengths differ, and how many lift
-- to no p-code at all.
SELECT form, count(*) n, sum(sleigh_length IS NULL) undecoded,
       sum(sleigh_length <> length) length_differs,
       sum(pcode = '') no_pcode
FROM decoder WHERE aligned = 1 GROUP BY form ORDER BY n DESC;

-- Static branch targets: how many land on an instruction start.
SELECT form, count(*) n, sum(target_aligned) on_an_instruction
FROM decoder
WHERE aligned = 1 AND target_sw IS NOT NULL
  AND target_sw BETWEEN (SELECT min(sw) FROM decoder) AND (SELECT max(sw) FROM decoder)
GROUP BY form ORDER BY n DESC;

-- Targets that land inside an instruction rather than on one, by the form
-- they land inside and how far into it. A form that swallows the words after
-- it shows up here.
WITH a AS (SELECT sw, form, length / 2 words FROM decoder WHERE aligned = 1)
SELECT b.form branch, a.form lands_inside, b.target_sw - a.sw words_in, count(*) n
FROM decoder b JOIN a ON b.target_sw > a.sw AND b.target_sw < a.sw + a.words
WHERE b.aligned = 1 AND b.target_aligned = 0
GROUP BY 1, 2, 3 ORDER BY n DESC;

-- Error bookmarks this run has and the attached one does not, with the
-- instruction the flow came from (needs ATTACH).
SELECT printf('%x', b.sw) sw, substr(b.text, 1, 60) text, printf('%x', b.flow_from_sw) src,
       d.form src_form, d.aligned src_aligned, d.cond,
       b.at_sw = d.target_sw at_is_src_target,
       b.at_sw = (SELECT i.fallthrough_sw FROM insn i WHERE i.sw = b.flow_from_sw) at_is_fallthrough
FROM bookmarks b LEFT JOIN decoder d ON d.sw = b.flow_from_sw
WHERE b.type = 'Error'
  AND NOT EXISTS (SELECT 1 FROM old.bookmarks o WHERE o.sw = b.sw AND o.text = b.text)
ORDER BY b.sw;

-- Decompiler warnings, this run against the attached one (needs ATTACH).
SELECT normalised,
       (SELECT count(*) FROM old.warnings o WHERE o.normalised = w.normalised) was,
       count(*) now
FROM warnings w GROUP BY normalised ORDER BY now DESC;

-- "overlaps instruction": the instruction the offcut address sits in, and what
-- jumps to it. addr1/addr2 are short words.
SELECT printf('%x', w.addr1) offcut, printf('%x', c.sw) inside, c.form,
       w.addr1 - c.sw words_in, r.type ref, printf('%x', r.from_sw) ref_from, s.form ref_form
FROM warnings w
LEFT JOIN decoder c ON w.addr1 > c.sw AND w.addr1 < c.sw + c.length / 2 AND c.aligned = 1
LEFT JOIN refs r ON r.to_sw = w.addr1
LEFT JOIN decoder s ON s.sw = r.from_sw
WHERE w.normalised LIKE '%overlaps%';

-- Main-program functions the decompiler truncates at bad instruction data.
SELECT printf('%x', f.sw) sw, f.instructions, d.seconds
FROM functions f JOIN decompiled d ON d.function_sw = f.sw
WHERE f.in_main = 1 AND d.c LIKE '%halt_baddata%'
ORDER BY f.instructions DESC LIMIT 20;
