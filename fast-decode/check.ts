// check.ts: the fast AV2 decoder must decode every file to the same bytes as
// pristine AVM. For each AV2 .avif (image item av02) in the given directories,
// pull out the item's OBUs, decode them with build/av2bench-pristine and with
// build/av2bench at 1 thread and at every core, and compare plane hashes. Exits
// 1 on any difference, and prints single-threaded times for both.
//
// usage: bun fast-decode/check.ts <dir> [<dir> ...]
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const HERE = import.meta.dir;
const FAST = path.join(HERE, "build/av2bench");
const PRISTINE = path.join(HERE, "build/av2bench-pristine");
for (const b of [FAST, PRISTINE]) if (!fs.existsSync(b)) throw new Error(`missing ${b}: run build.sh first`);

// The primary item's bytes, or null when the file isn't an AV2 image.
function av2Item(buf: Uint8Array): Uint8Array | null {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const str = (o: number) => String.fromCharCode(buf[o], buf[o + 1], buf[o + 2], buf[o + 3]);
  const boxes = (start: number, end: number) => {
    const out: { type: string; d: number; e: number }[] = [];
    for (let o = start; o + 8 <= end; ) {
      let size = dv.getUint32(o), hdr = 8;
      if (size === 1) { size = Number(dv.getBigUint64(o + 8)); hdr = 16; }
      if (size === 0) size = end - o;
      if (size < hdr || o + size > end) return out;
      out.push({ type: str(o + 4), d: o + hdr, e: o + size });
      o += size;
    }
    return out;
  };
  const meta = boxes(0, buf.length).find((x) => x.type === "meta");
  if (!meta) return null;
  const kids = boxes(meta.d + 4, meta.e);
  const get = (t: string) => kids.find((k) => k.type === t);
  const pitm = get("pitm"), iinf = get("iinf"), iloc = get("iloc");
  if (!pitm || !iinf || !iloc) return null;
  const primary = buf[pitm.d] === 0 ? dv.getUint16(pitm.d + 4) : dv.getUint32(pitm.d + 4);
  let type = "";
  for (const e of boxes(iinf.d + 4 + (buf[iinf.d] === 0 ? 2 : 4), iinf.e)) {
    const wide = buf[e.d] >= 3;
    const id = wide ? dv.getUint32(e.d + 4) : dv.getUint16(e.d + 4);
    if (id === primary) type = str(e.d + 4 + (wide ? 4 : 2) + 2);
  }
  if (type !== "av02") return null;
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

function run(bin: string, file: string, reps: number, threads: number) {
  const p = Bun.spawnSync([bin, file, String(reps), String(threads)], { stdout: "pipe", stderr: "pipe" });
  if (p.exitCode !== 0) throw new Error(`${path.basename(bin)} ${file}: ${p.stderr.toString().slice(-300)}`);
  const out = p.stdout.toString();
  return { hash: /hash ([0-9a-f]{16})/.exec(out)?.[1] ?? "", min: Number(/min ([\d.]+)/.exec(out)?.[1]) };
}

const dirs = process.argv.slice(2);
if (!dirs.length) {
  console.error("usage: bun fast-decode/check.ts <dir of AV2 .avif files> [...]");
  process.exit(2);
}
const work = fs.mkdtempSync(path.join(os.tmpdir(), "av2-fast-check-"));
const cores = os.availableParallelism();
let checked = 0, bad = 0;
try {
  for (const dir of dirs) {
    for (const f of fs.readdirSync(dir).filter((x) => x.endsWith(".avif")).sort()) {
      const item = av2Item(new Uint8Array(fs.readFileSync(path.join(dir, f))));
      if (!item) continue;
      const obu = path.join(work, `${f}.obu`);
      fs.writeFileSync(obu, item);
      const ref = run(PRISTINE, obu, 3, 1), one = run(FAST, obu, 3, 1), all = run(FAST, obu, 1, cores);
      const ok = ref.hash !== "" && ref.hash === one.hash && ref.hash === all.hash;
      checked++;
      if (!ok) bad++;
      console.log(`${ok ? "same" : "DIFF"}  ${f.padEnd(34)} pristine ${ref.min.toFixed(1)} ms, fast ${one.min.toFixed(1)} ms (1 thread)`);
    }
  }
} finally {
  fs.rmSync(work, { recursive: true, force: true });
}
console.log(`${checked - bad} of ${checked} AV2 files decode to the same bytes`);
if (!checked) {
  console.error("no AV2 files found");
  process.exit(2);
}
process.exit(bad ? 1 : 0);
