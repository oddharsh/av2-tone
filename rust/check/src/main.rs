//! av2-check: decode every AV2 .avif in the given directories with av2-tone
//! and with the AVM oracle (../oracle), and refuse any difference.
//!
//! Four checks per file:
//!   headers        av2-tone's parsed fields (src/dump.rs) against the same
//!                  fields read out of AVM, line for line
//!   tiles          each tile's offset and size in the stream
//!   frame context  the FRAME_CONTEXT every tile starts from, byte for byte
//!                  against AVM's (only its 2 alignment-padding bytes skipped)
//!   pixels         every 16-bit sample of every plane; the first difference
//!                  is printed as (plane, x, y) with both values
//!
//! With --entropy N it checks no files: it has the oracle encode N random
//! symbol streams with AVM's own entropy encoder and replays each through
//! av2-tone's decoder, which must recover every symbol, match AVM's tell and
//! tell_frac after every operation, and end with AVM's CDFs.
//!
//!   cargo run --release -p av2-check -- [--oracle <path>] [--headers] <dir>...
//!   cargo run --release -p av2-check -- [--oracle <path>] --entropy <streams> [--ops <n>]
//!   cargo run --release -p av2-check -- [--oracle <path>] --trace [--coverage] <dir>...
//!
//! With --trace it diffs av2-tone's tile parse against AVM's own symbol trace
//! (av2trace, built with AVM's accounting on): every symbol's value, coding
//! mode, cost and block must match, and the first that doesn't is named by
//! the AVM function and line that read it. --coverage then lists every AVM
//! read site the files reached, with its symbol count.
//!
//! Exits 1 on any difference, any decode error, or any panic in av2-tone (a
//! panic is a bug: the decoder must return an error instead). --headers lets
//! pixel differences through, for while reconstruction is still being built;
//! they are still reported, and every other check still fails the run.

#[path = "../../av2-tone/tests/common/entropy_script.rs"]
mod entropy_script;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use av2_tone::cdf::FrameContext;
use av2_tone::cdf_tables::FIELDS;

struct Oracle {
    /// width, height, bit_depth, ss_x, ss_y, monochrome
    picture: [u32; 6],
    headers: Vec<(String, String)>,
    /// (row, col, offset in the stream, size)
    tiles: Vec<[usize; 4]>,
    frame_context: Vec<u8>,
    planes: Vec<Vec<u16>>,
}

fn hex(s: &str) -> Result<Vec<u8>, String> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).unwrap_or("x"), 16).map_err(|e| e.to_string()))
        .collect()
}

fn run_oracle(bin: &Path, obu: &Path, planes: &Path) -> Result<Oracle, String> {
    let out = Command::new(bin).arg(obu).arg(planes).output().map_err(|e| format!("{}: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!("AVM failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut picture = None;
    let mut headers = Vec::new();
    let mut tiles = Vec::new();
    let mut frame_context = Vec::new();
    for line in text.lines() {
        let (k, v) = line.split_once(' ').unwrap_or((line, ""));
        if k == "picture" {
            let n: Vec<u32> = v.split(' ').filter_map(|x| x.parse().ok()).collect();
            picture = <[u32; 6]>::try_from(n).ok();
        } else if k == "tile" {
            let n: Vec<usize> = v.split(' ').filter_map(|x| x.parse().ok()).collect();
            tiles.push(<[usize; 4]>::try_from(n).map_err(|_| format!("bad tile line: {line}"))?);
        } else if k == "frame_context" {
            frame_context = hex(v)?;
        } else if k.starts_with("seq.") || k.starts_with("frame.") {
            headers.push((k.to_string(), v.to_string()));
        }
    }
    let picture = picture.ok_or("oracle printed no picture line")?;
    let raw = std::fs::read(planes).map_err(|e| format!("oracle planes: {e}"))?;
    let samples: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let [w, h, _, sx, sy, mono] = picture;
    let mut sizes = vec![(w * h) as usize];
    if mono == 0 {
        let c = (((w + sx) >> sx) * ((h + sy) >> sy)) as usize;
        sizes.extend([c, c]);
    }
    if sizes.iter().sum::<usize>() != samples.len() {
        return Err(format!("oracle wrote {} samples, expected {}", samples.len(), sizes.iter().sum::<usize>()));
    }
    let mut planes = Vec::new();
    let mut at = 0;
    for n in sizes {
        planes.push(samples[at..at + n].to_vec());
        at += n;
    }
    Ok(Oracle { picture, headers, tiles, frame_context, planes })
}

/// The first header field where the two disagree, or None.
fn diff_headers(ours: &str, avm: &[(String, String)]) -> Option<String> {
    let ours: Vec<(&str, &str)> = ours.lines().map(|l| l.split_once(' ').unwrap_or((l, ""))).collect();
    for (i, (k, v)) in avm.iter().enumerate() {
        match ours.get(i) {
            Some((ok, ov)) if ok == k && ov == v => {}
            Some((ok, ov)) if ok == k => return Some(format!("{k}: av2-tone \"{ov}\", AVM \"{v}\"")),
            Some((ok, _)) => return Some(format!("field order: av2-tone has {ok} where AVM has {k}")),
            None => return Some(format!("av2-tone stops before {k}")),
        }
    }
    (ours.len() > avm.len()).then(|| format!("av2-tone has extra field {}", ours[avm.len()].0))
}
/// "same", or where and how much the planes differ.
fn diff_pixels(pic: &av2_tone::Picture, avm: &Oracle) -> Result<(), String> {
    let [w, h, bd, sx, sy, _] = avm.picture;
    let ours = [pic.width, pic.height, pic.bit_depth, pic.subsampling_x, pic.subsampling_y];
    if ours != [w, h, bd, sx, sy] {
        return Err(format!("geometry {ours:?} against AVM's {:?}", [w, h, bd, sx, sy]));
    }
    if pic.planes.len() != avm.planes.len() {
        return Err(format!("{} planes against AVM's {}", pic.planes.len(), avm.planes.len()));
    }
    let mut first = None;
    let mut count = 0usize;
    for (p, (a, b)) in pic.planes.iter().zip(&avm.planes).enumerate() {
        let (pw, _) = pic.plane_size(p);
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            if x != y {
                count += 1;
                first.get_or_insert((p, i as u32 % pw, i as u32 / pw, *x, *y));
            }
        }
    }
    match first {
        None => Ok(()),
        Some((p, x, y, ours, theirs)) => {
            Err(format!("first at plane {p} ({x}, {y}): av2-tone {ours}, AVM {theirs}; {count} samples differ"))
        }
    }
}

/// The tiles' places in the stream, against AVM's.
fn diff_tiles(obu: &[u8], avm: &Oracle) -> Result<(), String> {
    let (h, group) = av2_tone::parse_obus(obu).map_err(|e| e.to_string())?;
    let ours: Vec<[usize; 4]> = av2_tone::tile::tile_buffers(&h, group.data)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|t| [t.row as usize, t.col as usize, group.offset + t.offset, t.data.len()])
        .collect();
    if ours != avm.tiles {
        return Err(format!("av2-tone {ours:?}, AVM {:?}", avm.tiles));
    }
    Ok(())
}

/// The starting frame context, byte for byte, naming the field of the first
/// difference.
fn diff_frame_context(headers: &av2_tone::Headers, avm: &Oracle) -> Result<usize, String> {
    let fc = FrameContext::default_for(headers.frame.quant.base_qindex, headers.seq.reduced_tx_part_set)
        .map_err(|e| e.to_string())?;
    let ours = fc.to_avm_bytes();
    if ours.len() != avm.frame_context.len() {
        return Err(format!("{} bytes, AVM's FRAME_CONTEXT is {}", ours.len(), avm.frame_context.len()));
    }
    let pad = FrameContext::avm_padding();
    let first = ours.iter().zip(&avm.frame_context).enumerate().find(|(i, (a, b))| a != b && !pad.contains(i));
    if let Some((byte, (a, b))) = first {
        let at = byte / 2;
        let field = FIELDS.iter().find(|(_, off, len)| (*off..off + len).contains(&at));
        return Err(match field {
            Some((name, off, _)) => format!("byte {byte}, {name}[{}]: av2-tone {a:#04x}, AVM {b:#04x}", at - off),
            None => format!("byte {byte} (initialized): av2-tone {a:#04x}, AVM {b:#04x}"),
        });
    }
    Ok(ours.len() - pad.len())
}

/// AVM's symbol trace for one stream: (name, x, y, tree, mode, value, bits).
struct AvmSym {
    name: String,
    s: av2_tone::entropy::TraceSym,
}

fn run_trace(bin: &Path, obu: &Path) -> Result<Vec<AvmSym>, String> {
    let out = Command::new(bin).arg(obu).output().map_err(|e| format!("{}: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!("AVM failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut names = BTreeMap::new();
    let mut raw = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        match f.first() {
            Some(&"name") if f.len() >= 4 => {
                names.insert(f[1].to_string(), f[2..].join(" "));
            }
            Some(&"sym") if f.len() == 8 => {
                let n = |i: usize| f[i].parse::<i64>().unwrap_or(-1);
                raw.push((
                    f[1].to_string(),
                    av2_tone::entropy::TraceSym {
                        x: n(2) as i32,
                        y: n(3) as i32,
                        tree: n(4) as u8,
                        mode: n(5) as u8,
                        value: n(6) as i32,
                        bits: f[7].parse().unwrap_or(0),
                    },
                ));
            }
            _ => {}
        }
    }
    Ok(raw.into_iter().map(|(id, s)| AvmSym { name: names.get(&id).cloned().unwrap_or(id), s }).collect())
}

fn show(s: &av2_tone::entropy::TraceSym) -> String {
    const MODES: [&str; 5] = ["bool", "bit", "literal", "unary", "cdf"];
    format!(
        "{} {} at mi ({}, {}) tree {}, {} bits",
        MODES.get(s.mode as usize).unwrap_or(&"?"),
        s.value,
        s.x,
        s.y,
        s.tree,
        s.bits as f64 / 65536.0
    )
}

/// av2-tone's parse against AVM's symbol trace, file by file.
fn trace(tracer: &Path, files: &BTreeMap<PathBuf, PathBuf>, work: &Path, coverage: bool) -> ExitCode {
    let (mut full, mut checked, mut matched_total, mut avm_total) = (0, 0, 0usize, 0usize);
    let mut sites: BTreeMap<String, usize> = BTreeMap::new();
    let mut failed = false;
    for path in files.values() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Ok(obu) = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|f| av2_tone::avif::primary_item(&f).map_err(|e| e.to_string()))
        else {
            continue;
        };
        checked += 1;
        let obu_path = work.join("item.obu");
        if std::fs::write(&obu_path, &obu).is_err() {
            continue;
        }
        let avm = match run_trace(tracer, &obu_path) {
            Ok(a) => a,
            Err(e) => {
                println!("ERROR {name}: {e}");
                failed = true;
                continue;
            }
        };
        let ours = match std::panic::catch_unwind(|| av2_tone::parse::trace_obus(&obu)) {
            Ok(t) => t,
            Err(_) => {
                println!("PANIC {name}: av2-tone panicked, which is a bug");
                failed = true;
                continue;
            }
        };
        for a in &avm {
            *sites.entry(a.name.clone()).or_default() += 1;
        }
        let same = ours.syms.iter().zip(&avm).take_while(|(a, b)| **a == b.s).count();
        matched_total += same;
        avm_total += avm.len();
        if same == avm.len() && ours.syms.len() == avm.len() && ours.result.is_ok() {
            full += 1;
            println!("{name:44} all {} symbols match", avm.len());
            continue;
        }
        failed = true;
        println!("{name:44} {same} of {} symbols match", avm.len());
        if let (Some(a), Some(o)) = (avm.get(same), ours.syms.get(same)) {
            println!("    symbol {same}: AVM read {} ({}), av2-tone {}", a.name, show(&a.s), show(o));
        } else if let Some(a) = avm.get(same) {
            let why = ours.result.as_ref().err().map_or("stopped".to_string(), |e| e.to_string());
            println!("    av2-tone {why}; AVM's next read is {} ({})", a.name, show(&a.s));
        } else {
            println!("    av2-tone read {} symbols past AVM's last", ours.syms.len() - same);
        }
    }
    println!(
        "{checked} files: {full} parse identically to AVM; {matched_total} of {avm_total} symbols match ({:.1}%)",
        100.0 * matched_total as f64 / avm_total.max(1) as f64
    );
    if coverage {
        for (site, n) in &sites {
            println!("{n:>10}  {site}");
        }
        println!("{} AVM read sites reached", sites.len());
    }
    if failed || checked == 0 { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn entropy(oracle: &Path, streams: u32, ops: u32) -> ExitCode {
    let mut all = entropy_script::Coverage::default();
    let mut failed = 0;
    for seed in 1..=streams {
        let out = match Command::new(oracle).args(["--entropy", &seed.to_string(), &ops.to_string()]).output() {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            Ok(o) => {
                println!("ERROR seed {seed}: {}", String::from_utf8_lossy(&o.stderr).trim());
                failed += 1;
                continue;
            }
            Err(e) => {
                eprintln!("{}: {e}", oracle.display());
                return ExitCode::from(2);
            }
        };
        match std::panic::catch_unwind(|| entropy_script::replay(&out)) {
            Ok(Ok(cov)) => all.add(&cov),
            Ok(Err(e)) => {
                println!("DIFF seed {seed}: {e}");
                failed += 1;
            }
            Err(_) => {
                println!("PANIC seed {seed}: av2-tone panicked, which is a bug");
                failed += 1;
            }
        }
    }
    println!("alphabet  symbols (no adaptation)  symbols (adapting)");
    for n in 2..=16 {
        let [off, on] = all.symbols[n];
        println!("{n:>8}  {off:>23}  {on:>18}");
    }
    println!(
        "{streams} AVM streams, {} operations ({} bool/literal/bit/unary): {} match, {failed} differ",
        all.ops,
        all.other,
        streams - failed
    );
    let every_size = (2..=16).all(|n| all.symbols[n][0] > 0 && all.symbols[n][1] > 0);
    if !every_size {
        println!("not every alphabet size was covered both ways; run more streams");
    }
    if failed > 0 || !every_size { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut oracle = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/av2oracle"));
    let mut headers_only = false;
    let mut entropy_streams = None;
    let mut trace_mode = false;
    let mut coverage = false;
    let mut ops = 4000;
    let mut dirs = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--oracle" => oracle = args.next().map(PathBuf::from).unwrap_or_default(),
            "--headers" => headers_only = true,
            "--trace" => trace_mode = true,
            "--coverage" => coverage = true,
            "--entropy" => entropy_streams = args.next().and_then(|n| n.parse::<u32>().ok()),
            "--ops" => ops = args.next().and_then(|n| n.parse().ok()).unwrap_or(ops),
            _ => dirs.push(PathBuf::from(a)),
        }
    }
    if !oracle.exists() {
        eprintln!("missing {}: run rust/oracle/build.sh", oracle.display());
        return ExitCode::from(2);
    }
    if let Some(n) = entropy_streams {
        return entropy(&oracle, n, ops);
    }
    if dirs.is_empty() {
        eprintln!("usage: av2-check [--oracle <path>] [--headers] <dir of AV2 .avif files>...");
        eprintln!("       av2-check [--oracle <path>] --entropy <streams> [--ops <n>]");
        return ExitCode::from(2);
    }
    let work = std::env::temp_dir().join(format!("av2-check-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&work) {
        eprintln!("{}: {e}", work.display());
        return ExitCode::from(2);
    }

    let mut files = BTreeMap::new();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            eprintln!("cannot read {}", dir.display());
            return ExitCode::from(2);
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "avif") {
                files.insert(p.clone(), p);
            }
        }
    }

    if trace_mode {
        let tracer = oracle.with_file_name("av2trace");
        if !tracer.exists() {
            eprintln!("missing {}: run rust/oracle/build.sh", tracer.display());
            return ExitCode::from(2);
        }
        let code = trace(&tracer, &files, &work, coverage);
        let _ = std::fs::remove_dir_all(&work);
        return code;
    }

    let mut checked = 0;
    let (mut header_fail, mut tile_fail, mut fc_fail, mut pixel_fail, mut errors) = (0, 0, 0, 0, 0);
    let mut buckets = [0usize; 4];
    for path in files.values() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let file = match std::fs::read(path) {
            Ok(f) => f,
            Err(e) => {
                println!("ERROR {name}: {e}");
                errors += 1;
                continue;
            }
        };
        let obu = match av2_tone::avif::primary_item(&file) {
            Ok(o) => o,
            Err(av2_tone::Error::NotAv2) => continue,
            Err(e) => {
                println!("ERROR {name}: container: {e}");
                errors += 1;
                continue;
            }
        };
        checked += 1;
        let obu_path = work.join("item.obu");
        let planes_path = work.join("planes.bin");
        if let Err(e) = std::fs::write(&obu_path, &obu) {
            println!("ERROR {name}: {e}");
            errors += 1;
            continue;
        }
        let avm = match run_oracle(&oracle, &obu_path, &planes_path) {
            Ok(a) => a,
            Err(e) => {
                println!("ERROR {name}: {e}");
                errors += 1;
                continue;
            }
        };
        let ours = std::panic::catch_unwind(|| av2_tone::decode_obus(&obu));
        let (headers, pic) = match ours {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                println!("ERROR {name}: av2-tone: {e}");
                errors += 1;
                continue;
            }
            Err(_) => {
                println!("PANIC {name}: av2-tone panicked, which is a bug");
                errors += 1;
                continue;
            }
        };
        buckets[av2_tone::cdf::q_ctx(headers.frame.quant.base_qindex)] += 1;
        let h = diff_headers(&av2_tone::dump::dump(&headers), &avm.headers);
        let t = diff_tiles(&obu, &avm).err();
        let fc = diff_frame_context(&headers, &avm);
        let p = diff_pixels(&pic, &avm).err();
        header_fail += usize::from(h.is_some());
        tile_fail += usize::from(t.is_some());
        fc_fail += usize::from(fc.is_err());
        pixel_fail += usize::from(p.is_some());
        println!("{name}");
        println!(
            "    headers {}",
            h.as_deref().map_or_else(|| format!("same ({} fields)", avm.headers.len()), |d| format!("DIFF {d}"))
        );
        println!(
            "    tiles   {}",
            t.as_deref().map_or_else(|| format!("same ({} tiles)", avm.tiles.len()), |d| format!("DIFF {d}"))
        );
        println!("    fc      {}", fc.as_ref().map_or_else(|d| format!("DIFF {d}"), |n| format!("same ({n} bytes)")));
        println!("    pixels  {}", p.as_deref().map_or("same".to_string(), |d| format!("DIFF {d}")));
    }
    let _ = std::fs::remove_dir_all(&work);

    let ok = |fail: usize| checked - fail - errors;
    println!(
        "{checked} AV2 files, {errors} errors. Match: headers {}, tiles {}, frame context {}, pixels {}. q buckets {buckets:?}",
        ok(header_fail),
        ok(tile_fail),
        ok(fc_fail),
        ok(pixel_fail)
    );
    if checked == 0 {
        eprintln!("no AV2 files found");
        return ExitCode::from(2);
    }
    let failed = errors > 0 || header_fail + tile_fail + fc_fail > 0 || (!headers_only && pixel_fail > 0);
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

#[cfg(test)]
mod tests {
    use super::diff_headers;

    fn avm(lines: &[(&str, &str)]) -> Vec<(String, String)> {
        lines.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn header_diff_finds_the_first_difference() {
        let a = avm(&[("seq.id", "0"), ("frame.q", "153 0")]);
        assert_eq!(diff_headers("seq.id 0\nframe.q 153 0\n", &a), None);
        assert_eq!(
            diff_headers("seq.id 0\nframe.q 152 0\n", &a).as_deref(),
            Some("frame.q: av2-tone \"152 0\", AVM \"153 0\"")
        );
        assert!(diff_headers("seq.id 0\n", &a).is_some_and(|d| d.contains("stops before frame.q")));
        assert!(diff_headers("seq.id 0\nframe.q 153 0\nframe.x 1\n", &a).is_some_and(|d| d.contains("extra")));
        assert!(diff_headers("seq.id 0\nframe.r 153 0\n", &a).is_some_and(|d| d.contains("order")));
    }
}
