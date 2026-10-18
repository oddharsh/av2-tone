// corpus.ts: the AV2 stills the safe-Rust decoder is checked on. Every file is
// cut from ../fixture/gen.ts's frame and encoded by this repo's tuned avifenc
// at speed 6, the speed aadhar.sh ships, across what the decoder must read:
//
//   depth      8 and 10 bits
//   chroma     4:4:4 and 4:2:0
//   qp         40, 105 and 180 (AV2's -48..255 quantizer, through -a qp=)
//   config     base       speed 6 as the encoder leaves it
//              unlock     + tool-unlock=6 (TCQ and the IST/CCTX search, patch 0007)
//              qmseg      + aadhar.sh's QM flags (enable-qm, qm-curve, qmseg at level 12)
//              shipped    qmseg and unlock together, aadhar.sh's AV2_ARGS at speed 6
//
// That's 48 crops of 250x186, a size that is a multiple of neither 8 nor the
// 128px superblock, placed so it crosses all four of the fixture's quadrants
// and the edge of its disc, all with aadhar.sh's 128px superblocks. Four full
// 600x600 frames cover the tile grid: one at the shipped config, then 2x2
// tiles at superblock sizes 64, 128 and 256 (a key frame codes 256 as 128,
// which scales the grid). Three more crops: 8-bit 4:2:0 at 64px superblocks,
// alone and in 2x2 tiles, and one with reduced-tx-part-set, the one sequence
// flag the default CDFs depend on. Two more turn trellis quantization off,
// which takes TCQ out of the sequence header: the only way a still gets
// parity hiding, since AVM forces TCQ on for a single picture whenever the
// sequence allows it. 57 files in all.
//
// Deterministic: the fixture is seeded, the crop is fixed, 8-bit is the 10-bit
// sample rounded once, and 4:2:0 chroma is the rounded mean of each 2x2.
//
//   bun rust/corpus.ts [out dir]     default rust/build/corpus
//
// Needs ../encoder/build.sh --tuned (qmseg and tool-unlock are tuned-build
// options). Writes <name>.avif per file plus corpus.json, the flags behind each.
import fs from "node:fs";
import path from "node:path";

const HERE = import.meta.dir;
const AVIFENC = process.env.AVIFENC ?? path.join(HERE, "../encoder/build/tuned/avifenc");
const OUT = path.resolve(process.argv[2] ?? path.join(HERE, "build/corpus"));
if (!fs.existsSync(AVIFENC)) {
  console.error(`missing ${AVIFENC}: run encoder/build.sh --tuned, or set AVIFENC`);
  process.exit(2);
}

const BASE = ["-c", "avm", "--speed", "6", "-q", "50", "--jobs", "4"];
const SB128 = ["-a", "sb-size=128"];
const TILES2X2 = ["--tilerowslog2", "1", "--tilecolslog2", "1"];
const QM = ["-a", "enable-qm=1", "-a", "qm-curve=1", "-a", "qmseg=1", "-a", "qmseg-level=12"];
const UNLOCK = ["-a", "tool-unlock=6"];
const CONFIGS: Record<string, string[]> = {
  base: [],
  unlock: UNLOCK,
  qmseg: QM,
  shipped: [...QM, ...UNLOCK],
};
const QPS = [40, 105, 180];
const CROP = { x: 175, y: 207, w: 250, h: 186 };

type Frame = { w: number; h: number; depth: 8 | 10; sub: 444 | 420; planes: Uint16Array[] };

function readFixture(file: string): Frame {
  const buf = fs.readFileSync(file);
  const nl = buf.indexOf(0x0a);
  const head = buf.subarray(0, nl).toString();
  const w = Number(/ W(\d+)/.exec(head)?.[1]), h = Number(/ H(\d+)/.exec(head)?.[1]);
  if (!head.includes("C444p10")) throw new Error(`fixture is not 4:4:4 10-bit: ${head}`);
  const start = buf.indexOf(0x0a, nl + 1) + 1; // past "FRAME\n"
  const n = w * h;
  const planes = [0, 1, 2].map((p) => {
    const out = new Uint16Array(n);
    for (let i = 0; i < n; i++) out[i] = buf.readUInt16LE(start + 2 * (p * n + i));
    return out;
  });
  return { w, h, depth: 10, sub: 444, planes };
}

function crop(f: Frame, x0: number, y0: number, w: number, h: number): Frame {
  const planes = f.planes.map((p) => {
    const out = new Uint16Array(w * h);
    for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) out[y * w + x] = p[(y0 + y) * f.w + x0 + x];
    return out;
  });
  return { ...f, w, h, planes };
}

function to8(f: Frame): Frame {
  return { ...f, depth: 8, planes: f.planes.map((p) => p.map((v) => Math.min(255, (v + 2) >> 2))) };
}

function to420(f: Frame): Frame {
  const cw = (f.w + 1) >> 1, ch = (f.h + 1) >> 1;
  const sub = (p: Uint16Array) => {
    const out = new Uint16Array(cw * ch);
    for (let y = 0; y < ch; y++) {
      for (let x = 0; x < cw; x++) {
        let s = 0, k = 0;
        for (let dy = 0; dy < 2; dy++) for (let dx = 0; dx < 2; dx++) {
          const sx = 2 * x + dx, sy = 2 * y + dy;
          if (sx < f.w && sy < f.h) { s += p[sy * f.w + sx]; k++; }
        }
        out[y * cw + x] = Math.floor((s + (k >> 1)) / k);
      }
    }
    return out;
  };
  return { ...f, sub: 420, planes: [f.planes[0], sub(f.planes[1]), sub(f.planes[2])] };
}

function writeY4m(f: Frame, file: string) {
  const c = `C${f.sub}${f.depth === 10 ? "p10" : ""}`;
  const head = Buffer.from(`YUV4MPEG2 W${f.w} H${f.h} F25:1 Ip A1:1 ${c}\nFRAME\n`);
  const body = f.planes.map((p) => {
    if (f.depth === 8) return Buffer.from(Uint8Array.from(p));
    const b = Buffer.alloc(p.length * 2);
    p.forEach((v, i) => b.writeUInt16LE(v, 2 * i));
    return b;
  });
  fs.writeFileSync(file, Buffer.concat([head, ...body]));
}

const gen = Bun.spawnSync(["bun", path.join(HERE, "../fixture/gen.ts")], { stdout: "pipe", stderr: "inherit" });
if (gen.exitCode !== 0) throw new Error("fixture/gen.ts failed");
const fixture = readFixture(gen.stdout.toString().trim());

fs.mkdirSync(OUT, { recursive: true });
const work = path.join(OUT, "y4m");
fs.mkdirSync(work, { recursive: true });

const cut = crop(fixture, CROP.x, CROP.y, CROP.w, CROP.h);
const sources: Record<string, Frame> = {
  "10b444": cut,
  "10b420": to420(cut),
  "8b444": to8(cut),
  "8b420": to420(to8(cut)),
};

type Job = { name: string; src: string; depth: number; qp: number; config: string; args: string[] };
const jobs: Job[] = [];
for (const [tag, frame] of Object.entries(sources)) {
  const y4m = path.join(work, `crop-${tag}.y4m`);
  writeY4m(frame, y4m);
  for (const [config, extra] of Object.entries(CONFIGS)) {
    for (const qp of QPS) {
      const args = [...BASE, ...SB128, "-d", String(frame.depth), "-a", `qp=${qp}`, ...extra];
      jobs.push({ name: `crop-${tag}-${config}-qp${qp}`, src: y4m, depth: frame.depth, qp, config, args });
    }
  }
}
const full = path.join(work, "full-10b444.y4m");
writeY4m(fixture, full);
const fullJob = (name: string, config: string, extra: string[]) =>
  jobs.push({ name, src: full, depth: 10, qp: 105, config, args: [...BASE, "-d", "10", "-a", "qp=105", ...extra] });
fullJob("full-10b444-shipped-qp105", "shipped", [...SB128, ...CONFIGS.shipped]);
fullJob("full-10b444-shipped-qp105-tiles2x2", "shipped", [...SB128, ...TILES2X2, ...CONFIGS.shipped]);
fullJob("full-10b444-base-qp105-sb64-tiles2x2", "base", ["-a", "sb-size=64", ...TILES2X2]);
fullJob("full-10b444-base-qp105-sb256-tiles2x2", "base", ["-a", "sb-size=256", ...TILES2X2]);
jobs.push({
  name: "crop-10b420-base-qp105-rtps", src: path.join(work, "crop-10b420.y4m"), depth: 10, qp: 105, config: "base",
  args: [...BASE, ...SB128, "-d", "10", "-a", "qp=105", "-a", "reduced-tx-part-set=1"],
});
jobs.push({
  name: "crop-8b420-base-qp180-sb64-tiles2x2", src: path.join(work, "crop-8b420.y4m"), depth: 8, qp: 180, config: "base",
  args: [...BASE, "-a", "sb-size=64", ...TILES2X2, "-d", "8", "-a", "qp=180"],
});
jobs.push({
  name: "crop-8b420-base-qp105-sb64", src: path.join(work, "crop-8b420.y4m"), depth: 8, qp: 105, config: "base",
  args: [...BASE, "-a", "sb-size=64", "-d", "8", "-a", "qp=105"],
});
for (const [tag, depth, qp] of [["8b420", 8, 40], ["10b444", 10, 105]] as const) {
  jobs.push({
    name: `crop-${tag}-base-qp${qp}-notcq`, src: path.join(work, `crop-${tag}.y4m`), depth, qp, config: "base",
    args: [...BASE, ...SB128, "-d", String(depth), "-a", `qp=${qp}`, "-a", "enable-trellis-quant=0"],
  });
}

// Four encodes at a time, each on four threads.
const manifest: Record<string, { config: string; qp: number; args: string[] }> = {};
let next = 0, done = 0;
async function worker() {
  while (next < jobs.length) {
    const j = jobs[next++];
    const out = path.join(OUT, `${j.name}.avif`);
    manifest[`${j.name}.avif`] = { config: j.config, qp: j.qp, args: j.args };
    if (fs.existsSync(out)) { done++; continue; }
    const p = Bun.spawn([AVIFENC, ...j.args, j.src, out + ".tmp"], { stdout: "pipe", stderr: "pipe" });
    const err = await new Response(p.stderr).text();
    if ((await p.exited) !== 0) throw new Error(`${j.name}: ${err.slice(-400)}`);
    fs.renameSync(out + ".tmp", out);
    console.log(`${String(++done).padStart(2)}/${jobs.length}  ${j.name}.avif  ${fs.statSync(out).size} B`);
  }
}
await Promise.all([worker(), worker(), worker(), worker()]);
fs.writeFileSync(path.join(OUT, "corpus.json"), JSON.stringify({ avifenc: AVIFENC, crop: CROP, files: manifest }, null, 1) + "\n");
fs.rmSync(work, { recursive: true, force: true });
console.log(`${jobs.length} files in ${OUT}`);
