// gen.ts: write fixture/build/fixture.y4m, a deterministic 600x600 10-bit
// 4:4:4 test frame, for the encoder's byte-identity gate and the Metal GDF
// check. It stands in for a photograph, so the repo needs no image whose
// licence differs from its own; set AV2_GATE_IMAGE to use a real one instead.
//
// What it needs to exercise: smooth gradients (where AV2 picks large blocks and
// GDF does little), seeded noise (texture, which qmseg classifies as such and
// GDF's classes split on), hard edges and fine stripes (every gradient
// direction GDF measures), and chroma that varies on its own. Same bytes on
// every run and every machine: the PRNG is a seeded xorshift32, and nothing
// reads the clock or Math.random.
//
//   bun fixture/gen.ts            writes fixture/build/fixture.y4m, prints its path
import fs from "node:fs";
import path from "node:path";

const W = 600, H = 600, MAX = 1023;
const out = path.join(import.meta.dir, "build", "fixture.y4m");

let seed = 0x2a7f0c3d;
const rand = (): number => {
  seed ^= seed << 13; seed >>>= 0;
  seed ^= seed >>> 17;
  seed ^= seed << 5; seed >>>= 0;
  return seed / 0x100000000;
};
const clip = (v: number): number => Math.max(0, Math.min(MAX, Math.round(v)));

const planes = [0, 1, 2].map(() => new Uint16Array(W * H));
for (let y = 0; y < H; y++) {
  for (let x = 0; x < W; x++) {
    const qx = Math.floor(x / 300), qy = Math.floor(y / 300);
    let luma: number;
    if (qx === 0 && qy === 0) luma = 120 + (x + y) * 0.65;                         // smooth ramp
    else if (qx === 1 && qy === 0) luma = 512 + (rand() - 0.5) * 360;              // noise texture
    else if (qx === 0 && qy === 1) luma = ((x >> 3) + (y >> 5)) & 1 ? 860 : 180;   // hard edges
    else luma = 512 + 300 * Math.sin(x * 0.9) * Math.cos(y * 0.23) + (rand() - 0.5) * 40; // fine stripes
    // A soft disc across the centre, so every quadrant meets a curved edge too.
    const d = Math.hypot(x - W / 2, y - H / 2);
    if (d < 140) luma = luma * 0.4 + 600 * 0.6 + (d < 135 ? 0 : 30);
    planes[0][y * W + x] = clip(luma);
    planes[1][y * W + x] = clip(512 + 180 * Math.sin(x / 47) + (qx === 1 ? (rand() - 0.5) * 120 : 0));
    planes[2][y * W + x] = clip(512 + 180 * Math.cos(y / 61) - (qy === 1 ? 90 : 0));
  }
}

fs.mkdirSync(path.dirname(out), { recursive: true });
const header = Buffer.from(`YUV4MPEG2 W${W} H${H} F25:1 Ip A1:1 C444p10 XYSCSS=444P10\nFRAME\n`);
const body = Buffer.concat(planes.map((p) => Buffer.from(p.buffer)));
fs.writeFileSync(out, Buffer.concat([header, body]));
console.log(out);
