//! `patchkit uses set`: write a measured DSP cost into the `[uses.<device>]` table of a module.toml.
//!
//! The cost is the worst steady block of one voice (`dsp_cost_voice`, a machine) or of one process call (`dsp_cost_fx`, an effect)
//! that the conformance run measured (docs/plans/sdk.md section 9). The file is edited like a lock file: it changes only when a count
//! differs, and then only the lines of the keys given. Other text, comments, other keys and the order of the tables stay.

use regex_lite::Regex;

/// The keys `set` writes: key name and the flag that carries its value.
pub const KEYS: [(&str, &str); 2] = [
    ("dsp_cost_voice", "--dsp-cost-voice"),
    ("dsp_cost_fx", "--dsp-cost-fx"),
];

/// `text` with the lines `key = value` of `values` in the table `[uses.<dev>]`. The table is made after the last `[uses.*]` table (or at
/// the end of the file) when it is missing. Unchanged when every key has that value already.
pub fn set_table(text: &str, dev: &str, values: &[(&str, u64)]) -> String {
    let head = format!("[uses.{dev}]");
    let want: Vec<String> = values.iter().map(|(k, v)| format!("{k} = {v}")).collect();
    let lines: Vec<&str> = text.split('\n').collect();
    let is_table = |l: &str| l.trim_start().starts_with('[');
    let Some(start) = lines.iter().position(|l| l.trim() == head) else {
        // a new table follows the last `[uses.*]` table, else goes to the end
        let last = lines.iter().rposition(|l| l.trim().starts_with("[uses."));
        let Some(last) = last else {
            return format!(
                "{}\n\n{head}\n{}\n",
                text.trim_end_matches('\n'),
                want.join("\n")
            );
        };
        let mut at = last + 1;
        while at < lines.len() && !is_table(lines[at]) {
            at += 1;
        }
        while at > 0 && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        let mut out: Vec<&str> = lines[..at].to_vec();
        out.push("");
        out.push(&head);
        out.extend(want.iter().map(String::as_str));
        out.extend(&lines[at..]);
        return out.join("\n");
    };
    let mut end = start + 1;
    while end < lines.len() && !is_table(lines[end]) {
        end += 1;
    }
    let body = &lines[start + 1..end];
    let count = Regex::new(r"^\s*(\w+)\s*=\s*(\d+)\s*(?:#.*)?$").expect("a fixed pattern");
    let have = |key: &str| {
        body.iter()
            .filter_map(|l| count.captures(l))
            .find(|c| &c[1] == key)
            .map(|c| c[2].to_string())
    };
    if values
        .iter()
        .all(|(k, v)| have(k).as_deref() == Some(&v.to_string()))
    {
        return text.to_string();
    }
    // keep comments and blank lines of the table; the count lines take their place, missing ones follow the last count
    let key_of = Regex::new(r"^\s*(\w+)\s*=").expect("a fixed pattern");
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for l in body {
        let hit = key_of
            .captures(l)
            .and_then(|c| values.iter().find(|(k, _)| *k == &c[1]));
        match hit {
            Some((k, v)) => {
                seen.push(k);
                out.push(format!("{k} = {v}"));
            }
            None => out.push((*l).to_string()),
        }
    }
    let mut trail = 0;
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
        trail += 1;
    }
    for (k, v) in values {
        if !seen.contains(k) {
            out.push(format!("{k} = {v}"));
        }
    }
    out.extend(std::iter::repeat_n(String::new(), trail));
    let mut all: Vec<String> = lines[..=start].iter().map(|l| (*l).to_string()).collect();
    all.extend(out);
    all.extend(lines[end..].iter().map(|l| (*l).to_string()));
    all.join("\n")
}

#[cfg(not(target_arch = "wasm32"))]
mod cli {
    use super::{KEYS, set_table};
    use crate::build;
    use crate::error::{Error, Result};
    use std::path::Path;

    const USAGE: &str = "usage: patchkit uses set --id MODULE --device SHORT [--dsp-cost-voice N] [--dsp-cost-fx N]";

    fn flag(rest: &[String], name: &str) -> Option<String> {
        rest.iter()
            .position(|a| a == name)
            .and_then(|i| rest.get(i + 1).cloned())
    }

    /// `patchkit uses set ...`: writes the module's `module.toml` and prints `wrote PATH` or `unchanged PATH`.
    pub fn run(root: &Path, rest: &[String]) -> Result<()> {
        if rest.first().map(String::as_str) != Some("set") {
            return Err(Error(USAGE.into()));
        }
        let rest = &rest[1..];
        let id = flag(rest, "--id").ok_or_else(|| Error(USAGE.into()))?;
        let device = flag(rest, "--device").ok_or_else(|| Error(USAGE.into()))?;
        let mut values: Vec<(&str, u64)> = Vec::new();
        for (key, name) in KEYS {
            if let Some(v) = flag(rest, name) {
                let n = v
                    .parse::<u64>()
                    .map_err(|_| Error(format!("{name}: {v:?} is not a count")))?;
                values.push((key, n));
            }
        }
        if values.is_empty() {
            return Err(Error(USAGE.into()));
        }
        let dir = build::resolve_module(root, &id)?;
        let path = dir.join("module.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error(format!("{}: {e}", path.display())))?;
        let next = set_table(&text, &device, &values);
        if next == text {
            println!("unchanged {}", path.display());
        } else {
            std::fs::write(&path, next).map_err(|e| Error(format!("{}: {e}", path.display())))?;
            println!("wrote {}", path.display());
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use cli::run;

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "[module]\nid = \"x/y\"\n\n[uses.dt2]\n# written by module:check\nmachine_rows = 1\nrecord_bytes = 444   # measured\n\n[uses.dn2]\nmachine_rows = 1\n\n[[object]]\npath = \"a\"\n";

    #[test]
    fn a_missing_key_follows_the_last_count_and_the_comments_stay() {
        let out = set_table(BASE, "dt2", &[("dsp_cost_voice", 5546)]);
        assert_eq!(
            out,
            BASE.replace(
                "record_bytes = 444   # measured\n",
                "record_bytes = 444   # measured\ndsp_cost_voice = 5546\n"
            )
        );
    }

    #[test]
    fn an_existing_key_is_replaced_in_place_and_nothing_else_moves() {
        let once = set_table(BASE, "dt2", &[("dsp_cost_voice", 100)]);
        let twice = set_table(&once, "dt2", &[("dsp_cost_voice", 200)]);
        assert_eq!(twice, once.replace("= 100", "= 200"));
    }

    #[test]
    fn the_same_value_leaves_the_text_alone() {
        let once = set_table(BASE, "dn2", &[("dsp_cost_fx", 7)]);
        assert_ne!(once, BASE);
        assert_eq!(set_table(&once, "dn2", &[("dsp_cost_fx", 7)]), once);
        // a value that was written with a trailing comment counts as present
        let commented = once.replace("dsp_cost_fx = 7", "dsp_cost_fx = 7 # kept");
        assert_eq!(
            set_table(&commented, "dn2", &[("dsp_cost_fx", 7)]),
            commented
        );
    }

    #[test]
    fn a_missing_table_is_made_after_the_last_uses_table() {
        let out = set_table(BASE, "ex", &[("dsp_cost_fx", 9)]);
        assert_eq!(
            out,
            BASE.replace(
                "machine_rows = 1\n\n[[object]]",
                "machine_rows = 1\n\n[uses.ex]\ndsp_cost_fx = 9\n\n[[object]]"
            )
        );
    }

    #[test]
    fn a_file_without_uses_tables_gets_one_at_its_end() {
        let out = set_table("[module]\nid = \"a\"\n", "dt2", &[("dsp_cost_voice", 3)]);
        assert_eq!(
            out,
            "[module]\nid = \"a\"\n\n[uses.dt2]\ndsp_cost_voice = 3\n"
        );
    }

    #[test]
    fn two_keys_are_written_in_the_order_given() {
        let out = set_table(BASE, "dn2", &[("dsp_cost_voice", 1), ("dsp_cost_fx", 2)]);
        assert!(
            out.contains("[uses.dn2]\nmachine_rows = 1\ndsp_cost_voice = 1\ndsp_cost_fx = 2\n"),
            "{out}"
        );
    }
}
