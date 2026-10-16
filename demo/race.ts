// race.ts: a live decoder race over photos run.ts already encoded. No
// encoding, so it starts at once and takes about half a minute.
//
//   1. Every finished run in demo/build/runs (or the stems named) joins.
//   2. Photo by photo, every decoder decodes the same OBUs (best of --reps
//      after two warm-ups, in process), and the screen redraws after each
//      one: milliseconds per photo, and how each compares to ours.
//   3. AV2 decoders must agree on every pixel of every photo, or it stops.
//   4. Replay: each decoder "decodes the gallery" again, animated at its
//      measured speed in real time, so the finish order is what you'd see.
//
//   bun demo/race.ts [stem ...] [--threads 1] [--reps 3] [--slowmo 1]
//
// --threads 1 pins every decoder to one core (the fast decoder otherwise
// spreads its loop filter across cores). Binaries resolve as run.ts does:
// FAST_DECODER_BUILD for av2bench and av2bench-pristine, demo/build for the
// dav1d and dav2d benches.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const HERE = import.meta.dir;
const ROOT = path.dirname(HERE);
const FAST = process.env.FAST_DECODER_BUILD ?? path.join(ROOT, "fast-decode/build");
const RUNS = path.join(HERE, "build/runs");

const argv = process.argv.slice(2);
const flag = (name: string, dflt: number) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? Number(argv[i + 1]) : dflt;
};
const THREADS = flag("threads", 1), REPS = flag("reps", 3), SLOWMO = flag("slowmo", 1);
const named = argv.filter((a, i) => !a.startsWith("--") && !argv[i - 1]?.startsWith("--"));

type Lane = { name: string; bin: string; file: "av2" | "av2_420" | "av1"; group: string; ours?: boolean };
const G444 = "4:4:4, what aadhar.sh ships", G420 = "4:2:0, the only AV2 dav2d reads", GAV1 = "AV1 for scale, same photo at the same bytes";
const LANES: Lane[] = [
  { name: "AVM (reference)", bin: path.join(FAST, "av2bench-pristine"), file: "av2", group: G444 },
  { name: "fast (ours)", bin: path.join(FAST, "av2bench"), file: "av2", group: G444, ours: true },
  { name: "AVM (reference)", bin: path.join(FAST, "av2bench-pristine"), file: "av2_420", group: G420 },
  { name: "fast (ours)", bin: path.join(FAST, "av2bench"), file: "av2_420", group: G420, ours: true },
  { name: "dav2d", bin: path.join(HERE, "build/dav2dbench"), file: "av2_420", group: G420 },
  { name: "dav1d", bin: path.join(HERE, "build/dav1dbench"), file: "av1", group: GAV1 },
];
const missing = [...new Set(LANES.map((l) => l.bin))].filter((b) => !fs.existsSync(b));
if (missing.length) {
  for (const b of missing) console.error(`missing ${b}: set FAST_DECODER_BUILD, or run demo/build.sh`);
  process.exit(2);
}

// The primary item's payload (its OBUs) from an AVIF, for av01 or av02. The
// same parser as run.ts's.
function primaryItem(file: string): Uint8Array {
  const buf = new Uint8Array(fs.readFileSync(file));
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const str = (o: number) => String.fromCharCode(buf[o], buf[o + 1], buf[o + 2], buf[o + 3]);
  const boxes = (start: number, end: number) => {
    const found: { type: string; d: number; e: number }[] = [];
    for (let o = start; o + 8 <= end; ) {
      let size = dv.getUint32(o), hdr = 8;
      if (size === 1) { size = Number(dv.getBigUint64(o + 8)); hdr = 16; }
      if (size === 0) size = end - o;
      if (size < hdr || o + size > end) break;
      found.push({ type: str(o + 4), d: o + hdr, e: o + size });
      o += size;
    }
    return found;
  };
  const meta = boxes(0, buf.length).find((b) => b.type === "meta");
  if (!meta) throw new Error(`${file}: no meta box`);
  const kids = boxes(meta.d + 4, meta.e);
  const pitm = kids.find((k) => k.type === "pitm")!, iloc = kids.find((k) => k.type === "iloc")!;
  const primary = buf[pitm.d] === 0 ? dv.getUint16(pitm.d + 4) : dv.getUint32(pitm.d + 4);
  let o = iloc.d;
  const v = buf[o];
  o += 4;
  const offSize = buf[o] >> 4, lenSize = buf[o] & 15, baseSize = buf[o + 1] >> 4;
  const idxSize = v >= 1 ? buf[o + 1] & 15 : 0;
  o += 2;
  const rd = (n: number) => { let x = 0; for (let i = 0; i < n; i++) x = x * 256 + buf[o++]; return x; };
  const parts: Uint8Array[] = [];
  for (let i = 0, count = v < 2 ? rd(2) : rd(4); i < count; i++) {
    const id = v < 2 ? rd(2) : rd(4);
    if (v >= 1) rd(2);
    rd(2);
    const base = rd(baseSize);
    for (let j = 0, n = rd(2); j < n; j++) {
      if (v >= 1 && idxSize) rd(idxSize);
      const off = rd(offSize), len = rd(lenSize);
      if (id === primary) parts.push(buf.subarray(base + off, base + off + len));
    }
  }
  return Buffer.concat(parts);
}

// 1. Photos: runs whose encodes finished. run.ts writes av2.avif and
// av2-420.avif only in its encoder race, the last encode step.
const stems = (named.length ? named : fs.readdirSync(RUNS)).filter((s) =>
  ["source.png", "av2.avif", "av2-420.avif", "av1.avif"].every((f) => fs.existsSync(path.join(RUNS, s, f))));
if (!stems.length) {
  console.error(`no finished runs in ${RUNS}: run bun demo/run.ts <photo> first`);
  process.exit(2);
}
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "av2-race-"));
const photos = await Promise.all(stems.map(async (stem) => {
  const dir = path.join(RUNS, stem);
  const source = await new Bun.Image(await Bun.file(path.join(dir, "source.png")).bytes()).metadata();
  const obu = (avif: string, name: string) => {
    const f = path.join(tmp, `${stem}.${name}.obu`);
    fs.writeFileSync(f, primaryItem(path.join(dir, avif)));
    return f;
  };
  return { stem, w: source.width as number, h: source.height as number,
    files: { av2: obu("av2.avif", "av2"), av2_420: obu("av2-420.avif", "av2_420"), av1: obu("av1.avif", "av1") } };
}));

// Drawing. A TTY gets a redrawn screen; anything else gets the final table.
const TTY = process.stdout.isTTY;
const ESC = "\x1b[";
const dim = (s: string) => (TTY ? `${ESC}2m${s}${ESC}0m` : s);
const bold = (s: string) => (TTY ? `${ESC}1m${s}${ESC}0m` : s);
const green = (s: string) => (TTY ? `${ESC}32m${s}${ESC}0m` : s);
const restore = () => { if (TTY) process.stdout.write(`${ESC}?25h`); };
process.on("exit", () => { restore(); fs.rmSync(tmp, { recursive: true, force: true }); });
process.on("SIGINT", () => process.exit(130));
if (TTY) process.stdout.write(`${ESC}?25l${ESC}2J`);
// Bars take what the terminal leaves: the live table's text is about 60
// columns, the replay's about 36.
const COLS = process.stdout.columns ?? 100;
const BAR = Math.max(8, Math.min(32, COLS - 60)), RBAR = Math.max(8, Math.min(48, COLS - 36));
const bar = (frac: number, width = BAR) => {
  const cells = Math.max(0, Math.min(width * 8, Math.round(frac * width * 8)));
  return ("█".repeat(cells >> 3) + (cells & 7 ? "▏▎▍▌▋▊▉"[(cells & 7) - 1] : "")).padEnd(width);
};
const header = () => {
  const l = os.loadavg()[0];
  return `${bold("AV2 decoder race")}  ${THREADS === 1 ? "one core each" : `${THREADS} threads each`}  ` +
    `${photos.length} photo${photos.length === 1 ? "" : "s"}  best of ${REPS}  ` +
    dim(`${os.cpus()[0]?.model}, load ${l.toFixed(0)} on ${os.availableParallelism()} cores${l > os.availableParallelism() ? " (busy: numbers will run slow)" : ""}`);
};

// 2. The race.
const times = LANES.map(() => [] as number[]);
const hashes = new Map<string, Set<string>>();
const ONE_CORE = { AVM_GDF_SERIAL: "1", AVM_LF_PIPELINE: "0" };
const mean = (xs: number[]) => xs.reduce((a, x) => a + x, 0) / xs.length;
const oursIn = (g: string) => LANES.findIndex((l) => l.ours && (l.group === g || (g === GAV1 && l.group === G444)));

function draw(status: string) {
  const out: string[] = [header(), status, ""];
  const avg = LANES.map((_, i) => (times[i].length ? mean(times[i]) : NaN));
  const scale = Math.max(...avg.filter(Number.isFinite), 1);
  let group = "";
  LANES.forEach((l, i) => {
    if (l.group !== group) { if (group) out.push(""); group = l.group; out.push(dim(`  ${group}`)); }
    const a = avg[i];
    let vs = "";
    const o = oursIn(l.group);
    if (!l.ours && Number.isFinite(a) && Number.isFinite(avg[o])) {
      const r = a / avg[o];
      vs = r >= 1 ? green(`ours ${r.toFixed(1)}x faster`) : `ours ${(1 / r).toFixed(1)}x slower`;
    }
    const name = l.ours ? bold(l.name.padEnd(16)) : l.name.padEnd(16);
    const ms = Number.isFinite(a) ? `${a.toFixed(1).padStart(6)} ms/photo` : "         waiting";
    out.push(`    ${name} ${bar(Number.isFinite(a) ? a / scale : 0)}  ${ms}  ${vs}`);
  });
  const checked = [...hashes.values()].filter((s) => s.size === 1).length;
  out.push("", dim(`  shorter bar = faster.  pixels identical across AV2 decoders: ${checked} of ${hashes.size} checks`));
  if (TTY) process.stdout.write(`${ESC}H${out.map((s) => s + `${ESC}K`).join("\n")}${ESC}J`);
  return out.join("\n");
}

for (const [p, photo] of photos.entries()) {
  // Rotate the starting lane per photo, so a burst of load from elsewhere
  // doesn't always land on the same decoder.
  for (let k = 0; k < LANES.length; k++) {
    const i = (k + p) % LANES.length, l = LANES[i];
    draw(`photo ${p + 1} of ${photos.length}: ${photo.stem} ${photo.w}x${photo.h}   now: ${l.name} on ${l.file === "av1" ? "AV1" : l.file === "av2" ? "AV2 4:4:4" : "AV2 4:2:0"}`);
    const r = Bun.spawnSync([l.bin, photo.files[l.file], String(REPS), String(THREADS)], {
      stdout: "pipe", stderr: "pipe", env: { ...process.env, ...(THREADS === 1 ? ONE_CORE : {}) },
    });
    const s = r.stdout.toString();
    const m = /hash ([0-9a-f]+) threads=\d+ median ([\d.]+) ms min ([\d.]+) ms/.exec(s);
    if (r.exitCode !== 0 || !m) throw new Error(`${l.name} on ${photo.stem} ${l.file}: ${s}${r.stderr.toString().slice(-300)}`);
    if (s.includes("UNSTABLE")) throw new Error(`${l.name} decoded ${photo.stem} ${l.file} to different pixels across reps`);
    times[i].push(+m[3]);
    // 3. Every AV2 decoder of the same file must land on the same pixels.
    if (l.file !== "av1") {
      const key = `${photo.stem}/${l.file}`;
      const set = hashes.get(key) ?? new Set<string>();
      set.add(m[1]);
      hashes.set(key, set);
      if (set.size > 1) throw new Error(`AV2 decoders disagree on ${key}: ${[...set].join(" vs ")}`);
    }
  }
}
const final = draw(`done: ${photos.map((p) => p.stem).join(", ")}`);
if (!TTY) console.log(final);

// 4. Replay: each lane decodes the whole gallery back to back at its measured
// per-photo times, animated against the wall clock (times --slowmo).
const totals = times.map((t) => t.reduce((a, x) => a + x, 0));
const longest = Math.max(...totals) * SLOWMO;
if (TTY) {
  await Bun.sleep(2500); // a moment to read the table before the replay takes the screen
  const title = bold(`Replay: each decoder decodes all ${photos.length} photo${photos.length === 1 ? "" : "s"}, back to back, in real time${SLOWMO !== 1 ? ` (slowed ${SLOWMO}x)` : ""}`);
  // Redrawn from the top of the screen each frame, like the live table, so a
  // narrow terminal that wraps a line can't knock the frame out of place.
  const frame = (elapsed: number) => [header(), title, "", ...LANES.map((l, i) => {
    const t = elapsed / SLOWMO;
    let done = 0, acc = 0;
    for (const x of times[i]) { if (acc + x <= t) { acc += x; done++; } else { done += (t - acc) / x; break; } }
    const tag = `${l.name.replace(" (reference)", "")} ${l.file === "av1" ? "AV1" : l.file === "av2" ? "4:4:4" : "4:2:0"}`;
    const label = l.ours ? bold(tag.padEnd(17)) : tag.padEnd(17);
    const right = t >= totals[i] ? green(`done ${totals[i].toFixed(0).padStart(5)} ms`) : `${t.toFixed(0).padStart(10)} ms`;
    return `  ${label} ${bar(Math.min(1, done / photos.length), RBAR)}  ${right}`;
  })].map((s) => s + `${ESC}K`).join("\n") + `${ESC}J`;
  process.stdout.write(`${ESC}2J`);
  const t0 = performance.now();
  for (;;) {
    const e = performance.now() - t0;
    process.stdout.write(`${ESC}H${frame(e)}`);
    if (e > longest + 250) break;
    await Bun.sleep(16);
  }
  console.log(`\n\n${final.split("\n").slice(2).join("\n")}`);
}
const f = oursIn(G444), a = LANES.findIndex((l) => l.group === G444 && !l.ours);
const f4 = oursIn(G420), d = LANES.findIndex((l) => l.name === "dav2d"), v1 = LANES.findIndex((l) => l.file === "av1");
console.log(`\n${bold("Verdict")} (${THREADS === 1 ? "one core" : `${THREADS} threads`}, ${photos.length} photo${photos.length === 1 ? "" : "s"}):`);
console.log(`  ours vs AVM on 4:4:4:   ${(totals[a] / totals[f]).toFixed(2)}x faster`);
const rd = totals[d] / totals[f4];
console.log(`  ours vs dav2d on 4:2:0: ${rd >= 1 ? `${rd.toFixed(2)}x faster` : `${(1 / rd).toFixed(2)}x slower`}`);
console.log(`  AV2 (ours) vs AV1 (dav1d): ${(totals[f] / totals[v1]).toFixed(1)}x slower to decode`);
