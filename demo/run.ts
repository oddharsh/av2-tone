// run.ts: encode one photo as AV1 and AV2 at the same file size, then race
// the encoders and decoders on it, and write what serve.ts shows.
//
//   1. The photo becomes an upright PNG, its long side --size px (Bun.Image
//      applies the EXIF orientation).
//   2. AV1 at the settings aadhar.sh ships (aom, -q 63, 10-bit 4:4:4, speed 2).
//      Its byte count is the budget.
//   3. AV2 with the tuned encoder, its quantizer searched until the file lands
//      within 3% under the budget, so the two images cost the same bytes.
//   4. Encoder race: pristine AVM against the tuned build, same arguments. The
//      tuned build's patch is speed only, so the two files must be identical.
//   5. Decoder race on the raw OBUs, best of --reps in three interleaved
//      rounds, at 1 thread (one core) and at
//      --threads: pristine AVM, the fast decoder, dav2d (on a 4:2:0 copy,
//      because dav2d refuses 4:4:4), and dav1d on the AV1 file for scale.
//      Every AV2 decoder must hash to the same pixels, or the run fails.
//   6. Both files decoded to PNG and scored against the source with
//      ssimulacra2 when it's installed (higher is better, 90 is visually
//      lossless).
//
//   bun demo/run.ts <photo> [--size 1600] [--speed 6] [--qp N] [--reps 9] [--threads 8]
//   bun demo/run.ts <photo> --decode-only    re-race the decoders on the last run's files
//
// Binaries come from this repo's builds unless overridden:
//   ENCODER_BUILD        default encoder/build (avifenc, avifdec, tuned/avifenc)
//   FAST_DECODER_BUILD   default fast-decode/build (av2bench, av2bench-pristine)
//   demo/build           dav1dbench, dav2dbench from demo/build.sh
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { inflateSync } from "node:zlib";

const HERE = import.meta.dir;
const ROOT = path.dirname(HERE);
const ENC = process.env.ENCODER_BUILD ?? path.join(ROOT, "encoder/build");
const FAST = process.env.FAST_DECODER_BUILD ?? path.join(ROOT, "fast-decode/build");
const BIN = {
  avifenc: path.join(ENC, "avifenc"),
  avifdec: path.join(ENC, "avifdec"),
  tunedenc: path.join(ENC, "tuned/avifenc"),
  avm: path.join(FAST, "av2bench-pristine"),
  fast: path.join(FAST, "av2bench"),
  dav2d: path.join(HERE, "build/dav2dbench"),
  dav1d: path.join(HERE, "build/dav1dbench"),
};
const HOW: Record<string, string> = {
  avifenc: "encoder/build.sh (or set ENCODER_BUILD)",
  avifdec: "encoder/build.sh (or set ENCODER_BUILD)",
  tunedenc: "encoder/build.sh --tuned (or set ENCODER_BUILD)",
  avm: "the fast decoder's build.sh (or set FAST_DECODER_BUILD)",
  fast: "the fast decoder's build.sh (or set FAST_DECODER_BUILD)",
  dav2d: "demo/build.sh",
  dav1d: "demo/build.sh",
};

const argv = process.argv.slice(2);
const flag = (name: string, dflt?: number) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? Number(argv[i + 1]) : dflt;
};
const takesValue = (a?: string) => !!a && a.startsWith("--") && a !== "--decode-only";
const photo = argv.find((a, i) => !a.startsWith("--") && !takesValue(argv[i - 1]));
if (!photo) {
  console.error("usage: bun demo/run.ts <photo> [--size 1600] [--speed 6] [--qp N] [--reps 9] [--threads 8]");
  process.exit(2);
}
const missing = Object.entries(BIN).filter(([, p]) => !fs.existsSync(p));
if (missing.length) {
  for (const [k, p] of missing) console.error(`missing ${p}: build it with ${HOW[k]}`);
  process.exit(2);
}
const SIZE = flag("size", 1600)!;
const SPEED = flag("speed", 6)!;
const REPS = flag("reps", 9)!;
const CORES = os.availableParallelism();
const THREADS = flag("threads", Math.min(8, CORES))!;
const JOBS = Math.min(8, CORES);
const stem = path.basename(photo).replace(/\.[^.]+$/, "");
const OUT = path.join(HERE, "build/runs", stem);
fs.mkdirSync(OUT, { recursive: true });
const out = (f: string) => path.join(OUT, f);
const load = () => os.loadavg()[0];
const say = (s: string) => console.log(s);

function sh(cmd: string[]): { ms: number; stdout: string } {
  const t0 = performance.now();
  const p = Bun.spawnSync(cmd, { stdout: "pipe", stderr: "pipe" });
  const ms = performance.now() - t0;
  if (p.exitCode !== 0) {
    throw new Error(`${path.basename(cmd[0])} exited ${p.exitCode ?? p.signalCode}: ${(p.stdout.toString() + p.stderr.toString()).slice(-400)}`);
  }
  return { ms, stdout: p.stdout.toString() };
}
// The same, without blocking, so encodes can run side by side.
async function shAsync(cmd: string[]): Promise<number> {
  const t0 = performance.now();
  const p = Bun.spawn(cmd, { stdout: "pipe", stderr: "pipe" });
  const [text, code] = await Promise.all([new Response(p.stdout).text(), p.exited]);
  if (code !== 0) throw new Error(`${path.basename(cmd[0])} exited ${code}: ${text.slice(-400)}`);
  return performance.now() - t0;
}
const bytes = (f: string) => fs.statSync(f).size;

// A monochrome camera's JPEG carries a GRAY ICC profile, and Bun.Image copies
// it into the RGB PNG it writes. The PNG spec forbids that pairing: libpng
// drops the profile with a warning (so avifenc encodes the pixels untagged)
// and ssimulacra2 refuses the file. Drop it here so every tool reads the same
// untagged pixels. Any other profile stays.
function dropGrayIcc(png: Uint8Array): Uint8Array {
  const dv = new DataView(png.buffer, png.byteOffset, png.byteLength);
  const type = (o: number) => String.fromCharCode(png[o + 4], png[o + 5], png[o + 6], png[o + 7]);
  const rgb = png[25] === 2 || png[25] === 6; // IHDR colour type: truecolour, with or without alpha
  for (let o = 8; o + 12 <= png.length; o += 12 + dv.getUint32(o)) {
    if (type(o) !== "iCCP") continue;
    const data = png.subarray(o + 8, o + 8 + dv.getUint32(o));
    const profile = inflateSync(data.subarray(data.indexOf(0) + 2)); // name, NUL, compression method
    if (!rgb || String.fromCharCode(...profile.subarray(16, 20)) !== "GRAY") return png;
    return Buffer.concat([png.subarray(0, o), png.subarray(o + 12 + data.length)]);
  }
  return png;
}

// The primary item's payload (its OBUs) from an AVIF, for av01 or av02.
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

const loadStart = load();
const srcPng = out("source.png"), av1 = out("av1.avif"), av2 = out("av2.avif"), av2_420 = out("av2-420.avif");
type Encoded = { dims: { width: number; height: number }; budget: number; av1Ms: number; qp: number; pristineMs: number; tunedMs: number };

// Steps 1 to 4. --decode-only skips them and reuses the last run's files and
// encode numbers, to re-race the decoders on a quieter machine.
async function encode(): Promise<Encoded> {
  // 1. Source
  const img = new Bun.Image(await Bun.file(photo).bytes());
  await Bun.write(srcPng, dropGrayIcc(await img.resize(SIZE, SIZE, { fit: "inside" }).png().bytes()));
  const dims = await new Bun.Image(await Bun.file(srcPng).bytes()).metadata();
  say(`${stem}: ${dims.width}x${dims.height}, load ${loadStart.toFixed(0)} on ${CORES} cores`);

  // 2. AV1, the budget
  const av1Enc = sh([BIN.avifenc, "-c", "aom", "-q", "63", "-d", "10", "--speed", "2", "--yuv", "444", "--jobs", String(JOBS), srcPng, av1]);
  const budget = bytes(av1);
  say(`AV1 (aom speed 2): ${budget} B in ${(av1Enc.ms / 1e3).toFixed(1)} s`);

  // 3. AV2 at the same bytes. File size falls about exponentially with qp
  // (measured: about 2.3% per qp step near the AV1 budget), so the first guess
  // uses that slope and later steps use the secant through the nearest two.
  const avm = (yuv: string) => ["-c", "avm", "-d", "10", "--speed", String(SPEED), "--yuv", yuv,
    "-a", "sb-size=128", "--jobs", String(JOBS)];
  const tries = new Map<number, number>();
  const encodeAt = (qp: number) => {
    if (!tries.has(qp)) {
      const ms = sh([BIN.tunedenc, ...avm("444"), "-a", `qp=${qp}`, srcPng, out(`av2-qp${qp}.avif`)]).ms;
      tries.set(qp, bytes(out(`av2-qp${qp}.avif`)));
      say(`  AV2 qp ${qp}: ${tries.get(qp)} B (${(tries.get(qp)! / budget * 100).toFixed(1)}% of AV1) in ${(ms / 1e3).toFixed(1)} s`);
    }
    return tries.get(qp)!;
  };
  let qp = flag("qp");
  if (qp === undefined) {
    const aim = Math.log(budget * 0.985);
    const fits = (q: number) => tries.get(q)! <= budget;
    const close = (q: number) => fits(q) && tries.get(q)! >= budget * 0.97;
    let next = 100;
    for (let i = 0; i < 6; i++) {
      encodeAt(next);
      if ([...tries.keys()].some(close)) break;
      const near = [...tries.keys()].sort((x, y) => Math.abs(Math.log(tries.get(x)!) - aim) - Math.abs(Math.log(tries.get(y)!) - aim));
      const [x, y] = near;
      const slope = y === undefined ? -0.023 : (Math.log(tries.get(y)!) - Math.log(tries.get(x)!)) / (y - x || 1);
      next = Math.max(0, Math.min(255, Math.round(x + (aim - Math.log(tries.get(x)!)) / (slope < 0 ? slope : -0.023))));
      while (tries.has(next) && next < 255) next++;
    }
    // the largest file that still fits
    qp = [...tries.keys()].filter(fits).sort((x, y) => tries.get(y)! - tries.get(x)!)[0];
    if (qp === undefined) throw new Error("no AV2 encode fit the AV1 budget; pass --qp");
  }
  for (const q of tries.keys()) fs.rmSync(out(`av2-qp${q}.avif`));

  // 4. Encoder race at the chosen qp, pristine AVM and the tuned build side by
  // side so both see the same load, with the 4:2:0 copy dav2d needs alongside.
  // The tuned build changes speed only, so its file must match pristine's.
  say(`encoder race at qp ${qp}, speed ${SPEED}, ${JOBS} jobs, side by side`);
  const pristine = out("av2-pristine.avif");
  const [pristineMs, tunedMs] = await Promise.all([
    shAsync([BIN.avifenc, ...avm("444"), "-a", `qp=${qp}`, srcPng, pristine]),
    shAsync([BIN.tunedenc, ...avm("444"), "-a", `qp=${qp}`, srcPng, av2]),
    shAsync([BIN.tunedenc, ...avm("420"), "-a", `qp=${qp}`, srcPng, av2_420]),
  ]);
  const sameBytes = Buffer.compare(fs.readFileSync(pristine), fs.readFileSync(av2)) === 0;
  fs.rmSync(pristine);
  if (!sameBytes) throw new Error("the tuned encoder wrote different bytes than pristine AVM");
  say(`  pristine AVM ${(pristineMs / 1e3).toFixed(1)} s, tuned ${(tunedMs / 1e3).toFixed(1)} s, identical files`);
  return { dims, budget, av1Ms: av1Enc.ms, qp: qp!, pristineMs, tunedMs };
}
let enc: Encoded;
if (argv.includes("--decode-only")) {
  const old = JSON.parse(fs.readFileSync(out("report.json"), "utf8"));
  enc = { dims: old.source, budget: old.av1.bytes, av1Ms: old.av1.encodeMs, qp: old.settings.qp,
    pristineMs: old.encode.pristineMs, tunedMs: old.encode.tunedMs };
  say(`${stem}: reusing the encodes in ${OUT}, load ${loadStart.toFixed(0)} on ${CORES} cores`);
} else {
  enc = await encode();
}
const { dims, budget, qp, pristineMs, tunedMs } = enc;

// 5. Decoder race
const obu = (avif: string) => {
  const f = avif.replace(/\.avif$/, ".obu");
  fs.writeFileSync(f, primaryItem(avif));
  return f;
};
const files = { av2: obu(av2), av2_420: obu(av2_420), av1: obu(av1) };
type Row = { decoder: string; file: string; threads: number; min: number; median: number; hash: string };
// One thread means one core: the fast decoder otherwise spreads GDF across
// cores and deblocks behind reconstruction even when told to use 1 thread.
const ONE_CORE = { AVM_GDF_SERIAL: "1", AVM_LF_PIPELINE: "0" };
const arms: [string, string, keyof typeof files][] = [
  ["AVM", BIN.avm, "av2"], ["fast", BIN.fast, "av2"],
  ["AVM", BIN.avm, "av2_420"], ["fast", BIN.fast, "av2_420"], ["dav2d", BIN.dav2d, "av2_420"],
  ["dav1d", BIN.dav1d, "av1"],
];
// The arms take turns over ROUNDS rounds, so a burst of load from elsewhere
// lands on all of them instead of on whichever ran during it.
const ROUNDS = 3, per = Math.max(1, Math.ceil(REPS / ROUNDS));
const rows: Row[] = [];
say(`decoder race, ${ROUNDS} rounds of ${per}, best overall`);
for (const t of [...new Set([1, THREADS])]) {
  const seen = arms.map(() => ({ mins: [] as number[], medians: [] as number[], hash: "" }));
  for (let r = 0; r < ROUNDS; r++) {
    arms.forEach(([decoder, bin, file], i) => {
      const p = Bun.spawnSync([bin, files[file], String(per), String(t)], {
        stdout: "pipe", stderr: "pipe", env: { ...process.env, ...(t === 1 ? ONE_CORE : {}) },
      });
      const s = p.stdout.toString();
      const m = /hash ([0-9a-f]+) threads=\d+ median ([\d.]+) ms min ([\d.]+) ms/.exec(s);
      if (p.exitCode !== 0 || !m) throw new Error(`${decoder} on ${file}: ${s}${p.stderr.toString().slice(-300)}`);
      if (seen[i].hash && seen[i].hash !== m[1]) throw new Error(`${decoder} on ${file} decoded two different pictures`);
      seen[i].hash = m[1];
      seen[i].medians.push(+m[2]);
      seen[i].mins.push(+m[3]);
    });
  }
  arms.forEach(([decoder, , file], i) => {
    const med = seen[i].medians.sort((x, y) => x - y)[ROUNDS >> 1];
    const row = { decoder, file, threads: t, hash: seen[i].hash, min: Math.min(...seen[i].mins), median: med };
    rows.push(row);
    say(`  ${decoder.padEnd(6)} ${file.padEnd(8)} ${String(t).padStart(2)} thr  best ${row.min.toFixed(1).padStart(6)} ms  median ${row.median.toFixed(1).padStart(6)} ms`);
  });
}
for (const f of ["av2", "av2_420"]) {
  const hashes = new Set(rows.filter((r) => r.file === f).map((r) => r.hash));
  if (hashes.size !== 1) throw new Error(`AV2 decoders disagree on ${f}: ${[...hashes].join(", ")}`);
}
say("every AV2 decoder wrote the same pixels");

// 6. Pictures and scores
sh([BIN.avifdec, av1, out("av1.png")]);
sh([BIN.avifdec, av2, out("av2.png")]);
const s2 = (png: string) => {
  if (!Bun.which("ssimulacra2")) return null;
  return Number(sh(["ssimulacra2", srcPng, png]).stdout.trim());
};
const scores = { av1: s2(out("av1.png")), av2: s2(out("av2.png")) };
if (scores.av1 !== null) say(`ssimulacra2: AV1 ${scores.av1.toFixed(2)}, AV2 ${scores.av2!.toFixed(2)}`);

const report = {
  stem,
  date: new Date().toISOString(),
  machine: { cpu: os.cpus()[0]?.model, cores: CORES, loadStart, loadEnd: load() },
  source: { width: dims.width, height: dims.height, png: "source.png", from: path.basename(photo) },
  settings: { speed: SPEED, qp, jobs: JOBS, reps: per * ROUNDS, threads: THREADS },
  av1: { file: "av1.avif", png: "av1.png", bytes: budget, encodeMs: enc.av1Ms, s2: scores.av1 },
  av2: { file: "av2.avif", png: "av2.png", bytes: bytes(av2), s2: scores.av2 },
  encode: { pristineMs, tunedMs, sameBytes: true },
  decode: rows,
};
fs.writeFileSync(out("report.json"), JSON.stringify(report, null, 2));
for (const f of Object.values(files)) fs.rmSync(f);
say(`wrote ${OUT}\nsee it: bun demo/serve.ts`);
