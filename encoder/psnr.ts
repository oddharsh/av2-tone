// Luma PSNR, in dB, of a decoded y4m against a source y4m (8-bit, or 10/12-bit
// little-endian samples). The build gate uses it to compare two encodes of the
// same fixture, so only the difference between two readings matters.
//   bun psnr.ts <source.y4m> <decoded.y4m>
import { readFileSync } from "node:fs";

function luma(file: string): Float64Array {
  const b = readFileSync(file);
  const nl = b.indexOf(10);
  const hdr = b.subarray(0, nl).toString();
  const w = Number(/ W(\d+)/.exec(hdr)?.[1]), h = Number(/ H(\d+)/.exec(hdr)?.[1]);
  const c = / C(\S+)/.exec(hdr)?.[1] ?? "420";
  const max = c.includes("p10") ? 1023 : c.includes("p12") ? 4095 : 255;
  const wide = max > 255;
  const start = b.indexOf(10, nl + 1) + 1; // past "FRAME\n"
  const y = new Float64Array(w * h);
  for (let i = 0; i < y.length; i++) y[i] = (wide ? b.readUInt16LE(start + 2 * i) : b[start + i]) / max;
  return y;
}

const [src, dec] = [luma(process.argv[2]), luma(process.argv[3])];
if (src.length !== dec.length) throw new Error("the two frames differ in size");
let se = 0;
for (let i = 0; i < src.length; i++) se += (src[i] - dec[i]) ** 2;
console.log((10 * Math.log10(src.length / se)).toFixed(2));
