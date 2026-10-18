//! av2-check: decode every AV2 .avif in the given directories with av2-tone
//! and with the AVM oracle (../oracle), and refuse any difference.
//!
//! Two checks per file. Headers: av2-tone's parsed fields (src/dump.rs) must
//! match the same fields read out of AVM, line for line; the first differing
//! field is printed. Pixels: every 16-bit sample of every plane must match
//! AVM's; the first differing sample is printed as (plane, x, y) with both
//! values, and the count of differing samples after it.
//!
//!   cargo run --release -p av2-check -- [--oracle <path>] [--headers] <dir>...
//!
//! Exits 1 on any difference, any decode error, or any panic in av2-tone (a
//! panic is a bug: the decoder must return an error instead). --headers makes
//! only the header check decide the exit status, for while reconstruction is
//! still being built; pixel differences are still reported.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

struct Oracle {
    /// width, height, bit_depth, ss_x, ss_y, monochrome
    picture: [u32; 6],
    headers: Vec<(String, String)>,
    planes: Vec<Vec<u16>>,
}

fn run_oracle(bin: &Path, obu: &Path, planes: &Path) -> Result<Oracle, String> {
    let out = Command::new(bin).arg(obu).arg(planes).output().map_err(|e| format!("{}: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!("AVM failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut picture = None;
    let mut headers = Vec::new();
    for line in text.lines() {
        let (k, v) = line.split_once(' ').unwrap_or((line, ""));
        if k == "picture" {
            let n: Vec<u32> = v.split(' ').filter_map(|x| x.parse().ok()).collect();
            picture = <[u32; 6]>::try_from(n).ok();
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
    Ok(Oracle { picture, headers, planes })
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

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut oracle = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/av2oracle"));
    let mut headers_only = false;
    let mut dirs = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--oracle" => oracle = args.next().map(PathBuf::from).unwrap_or_default(),
            "--headers" => headers_only = true,
            _ => dirs.push(PathBuf::from(a)),
        }
    }
    if dirs.is_empty() {
        eprintln!("usage: av2-check [--oracle <path>] [--headers] <dir of AV2 .avif files>...");
        return ExitCode::from(2);
    }
    if !oracle.exists() {
        eprintln!("missing {}: run rust/oracle/build.sh", oracle.display());
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

    let (mut checked, mut header_fail, mut pixel_fail, mut errors) = (0, 0, 0, 0);
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
        let h = diff_headers(&av2_tone::dump::dump(&headers), &avm.headers);
        let p = diff_pixels(&pic, &avm).err();
        header_fail += usize::from(h.is_some());
        pixel_fail += usize::from(p.is_some());
        println!(
            "{name:40} headers {}  pixels {}",
            h.as_deref().map_or_else(|| format!("same ({} fields)", avm.headers.len()), |d| format!("DIFF {d}")),
            p.as_deref().map_or("same".to_string(), |d| format!("DIFF {d}")),
        );
    }
    let _ = std::fs::remove_dir_all(&work);

    println!(
        "{checked} AV2 files: headers match on {}, pixels match on {}, {errors} errors",
        checked - header_fail - errors,
        checked - pixel_fail - errors
    );
    if checked == 0 {
        eprintln!("no AV2 files found");
        return ExitCode::from(2);
    }
    let failed = errors > 0 || header_fail > 0 || (!headers_only && pixel_fail > 0);
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
