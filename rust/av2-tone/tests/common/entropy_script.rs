//! Replays an `av2oracle --entropy` script through av2-tone's decoder. The
//! script holds the bytes AVM's encoder wrote, the CDFs it started from, every
//! operation with the value and AVM's tell and tell_frac after it, and the
//! CDFs AVM's reader ended with. The replay must match all of it.
//!
//! Shared by tests/entropy.rs (frozen scripts) and av2-check (fresh ones).

use av2_tone::entropy::SymbolReader;

/// What one replay covered, for reporting.
#[derive(Debug, Default, Clone)]
pub struct Coverage {
    pub ops: usize,
    /// Symbol reads by alphabet size 2..=16: [without adaptation, with].
    pub symbols: [[usize; 2]; 17],
    pub other: usize,
}

impl Coverage {
    pub fn add(&mut self, o: &Coverage) {
        self.ops += o.ops;
        self.other += o.other;
        for (a, b) in self.symbols.iter_mut().zip(o.symbols) {
            a[0] += b[0];
            a[1] += b[1];
        }
    }
}

fn nums(s: &str) -> Result<Vec<u64>, String> {
    s.split_whitespace().map(|x| x.parse::<u64>().map_err(|e| format!("{x}: {e}"))).collect()
}

fn hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd hex".into());
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string())).collect()
}

/// Replays one script; Err names the first operation that differs.
pub fn replay(script: &str) -> Result<Coverage, String> {
    let mut data = Vec::new();
    let mut cdfs: Vec<Vec<u16>> = vec![Vec::new(); 17];
    let mut cov = Coverage::default();
    let mut lines = script.lines().peekable();
    // Header: data and starting CDFs.
    while let Some(line) = lines.peek() {
        if line.starts_with("op ") {
            break;
        }
        let line = lines.next().unwrap_or_default();
        if let Some(h) = line.strip_prefix("data ") {
            data = hex(h)?;
        } else if let Some(c) = line.strip_prefix("cdf ") {
            let v = nums(c)?;
            let n = v[0] as usize;
            cdfs[n] = v[1..].iter().map(|&x| x as u16).collect();
        }
    }
    let mut r = SymbolReader::new(&data, true);
    for (i, line) in lines.enumerate() {
        if line == "end" {
            return Ok(cov);
        }
        if let Some(f) = line.strip_prefix("final ") {
            let v = nums(f)?;
            let n = v[0] as usize;
            let want: Vec<u16> = v[1..].iter().map(|&x| x as u16).collect();
            if cdfs[n] != want {
                return Err(format!("alphabet {n}: final CDF {:?}, AVM {want:?}", cdfs[n]));
            }
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let tail = |k: usize| nums(&f[f.len() - k..].join(" "));
        let [value, tell, frac] = <[u64; 3]>::try_from(tail(3)?).map_err(|_| "short op line")?;
        let got = match f.get(1).copied() {
            Some("sym") => {
                let n: usize = f[2].parse().map_err(|_| "alphabet")?;
                let adapt = f[3] == "1";
                r.allow_update_cdf = adapt;
                cov.symbols[n][usize::from(adapt)] += 1;
                r.read_symbol(&mut cdfs[n], n).map(|s| s as u64)
            }
            Some("bool") => {
                cov.other += 1;
                r.read_bool(f[2].parse().map_err(|_| "prob")?).map(u64::from)
            }
            Some("lit") => {
                cov.other += 1;
                r.read_literal(f[2].parse().map_err(|_| "bits")?).map(u64::from)
            }
            Some("bit") => {
                cov.other += 1;
                r.read_bit().map(u64::from)
            }
            Some("unary") => {
                cov.other += 1;
                r.read_unary(f[2].parse().map_err(|_| "max")?).map(u64::from)
            }
            _ => return Err(format!("unknown line {line}")),
        }
        .map_err(|e| format!("op {i} ({line}): {e}"))?;
        cov.ops += 1;
        let ours = (got, u64::from(r.tell()), r.tell_frac());
        if ours != (value, tell, frac) {
            return Err(format!(
                "op {i} `{}`: av2-tone value {} tell {} frac {}, AVM {value} {tell} {frac}",
                f[..f.len() - 3].join(" "),
                ours.0,
                ours.1,
                ours.2
            ));
        }
    }
    Err("script has no end line".into())
}
