// gen.ts: writes av2-tone/src/cdf_tables.rs, the default CDFs a key frame's
// tiles start from, out of AVM's own sources. Nothing in the output is typed
// by hand:
//
//   1. FRAME_CONTEXT's layout is parsed from av2/common/entropymode.h (with
//      nmv_context from entropymv.h and segmentation_probs from
//      seg_common.h): every CDF array, in declaration order.
//   2. A C probe, compiled against the oracle's pristine AVM build, has the
//      compiler evaluate each array's dimensions and offset, and checks that
//      the CDFs tile the struct with no gaps, only alignment padding before
//      the trailing `int initialized`.
//   3. The probe runs AVM's own av2_setup_past_independence() for every input
//      the defaults depend on: the base_qindex bucket (4, av2_default_coef_probs)
//      and reduced_tx_part_set (init_mode_probs). It prints every value. The
//      values are what AVM computes from entropy_inits_coeffs.h,
//      entropy_inits_modes.h, entropy_inits_mv.h and entropy_sideinfo.h, so
//      the AVM_CDFn / AVM_PARAn macros are expanded by AVM's compiler, not here.
//   4. get_q_ctx's thresholds are read out of av2/common/entropy.c.
//
// Refuses to run against an AVM tree that is dirty or not at the pinned v1.0.0
// commit, as ../oracle/build.sh refuses a dirty one.
//
//   rust/oracle/build.sh && bun rust/gen/cdf.ts       (AVM_SRC=<dir> to override)
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const AVM_COMMIT = "966a7d7cd6fcf60360caf5dc413b2aeeb65e144d"; // AVM v1.0.0, as encoder/build.sh's libavif pins it
const HERE = import.meta.dir;
const AVM_SRC = path.resolve(process.env.AVM_SRC ?? path.join(HERE, "../../encoder/build/_deps/libavm-src"));
const AVM_BUILD = path.join(HERE, "../build/avm");
const OUT = path.join(HERE, "../av2-tone/src/cdf_tables.rs");

function sh(cmd: string[], cwd?: string): string {
  const p = Bun.spawnSync(cmd, { cwd, stdout: "pipe", stderr: "pipe" });
  if (p.exitCode !== 0) throw new Error(`${cmd.join(" ")}:\n${p.stderr.toString().slice(-2000)}`);
  return p.stdout.toString();
}
function die(msg: string): never {
  console.error(`error: ${msg}`);
  process.exit(1);
}

if (!fs.existsSync(path.join(AVM_SRC, "av2/common/entropymode.h"))) die(`no AVM source at ${AVM_SRC}; set AVM_SRC`);
const head = sh(["git", "-C", AVM_SRC, "rev-parse", "HEAD"]).trim();
if (head !== AVM_COMMIT) die(`${AVM_SRC} is at ${head}, not AVM v1.0.0 (${AVM_COMMIT})`);
if (sh(["git", "-C", AVM_SRC, "status", "--porcelain", "--untracked-files=no"]).trim())
  die(`${AVM_SRC} has local changes; the tables must come from pristine AVM`);
if (!fs.existsSync(path.join(AVM_BUILD, "libavm.a"))) die(`no ${AVM_BUILD}/libavm.a: run rust/oracle/build.sh`);

// ---- 1. the layout --------------------------------------------------------
const read = (f: string) => fs.readFileSync(path.join(AVM_SRC, f), "utf8");
const stripC = (s: string) =>
  s.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "").replace(/^\s*#.*$/gm, "");

// A typedef'd struct is found by its unique closing name, then the nearest
// opening before it; a named struct by its opening, then the next close.
function body(src: string, open: string, close: string): string {
  const named = close === "};";
  const start = named ? src.indexOf(open) : src.lastIndexOf(open, src.indexOf(close));
  const end = named ? src.indexOf(close, start) : src.indexOf(close);
  if (start < 0 || end < 0) die(`cannot find ${close} in AVM's headers`);
  return src.slice(start + open.length, end);
}

type Decl = { type: string; name: string; dims: string[] };
function decls(text: string): Decl[] {
  return stripC(text)
    .split(";")
    .map((d) => d.replace(/\s+/g, " ").trim())
    .filter(Boolean)
    .map((d) => {
      const m = /^(struct \w+|\w+) (\w+)((?: ?\[[^\]]*\])*)$/.exec(d);
      if (!m) die(`unexpected declaration in FRAME_CONTEXT: ${d}`);
      return { type: m[1], name: m[2], dims: [...m[3].matchAll(/\[([^\]]*)\]/g)].map((x) => x[1].trim()) };
    });
}

const structs: Record<string, Decl[]> = {
  FRAME_CONTEXT: decls(body(read("av2/common/entropymode.h"), "typedef struct frame_contexts {", "} FRAME_CONTEXT;")),
  nmv_context: decls(body(read("av2/common/entropymv.h"), "typedef struct {", "} nmv_context;")),
  nmv_component: decls(body(read("av2/common/entropymv.h"), "typedef struct {", "} nmv_component;")),
  "struct segmentation_probs": decls(body(read("av2/common/seg_common.h"), "struct segmentation_probs {", "};")),
};

// Leaves: every avm_cdf_prob array, with its C member path. Arrays of structs
// are unrolled so each leaf has a constant offsetof.
type Leaf = { path: string; name: string; rank: number };
const leaves: Leaf[] = [];
let sawInitialized = false;
function flatten(type: string, cpath: string, rname: string) {
  const fields = structs[type];
  if (!fields) die(`no definition for ${type}`);
  for (const f of fields) {
    const p = cpath ? `${cpath}.${f.name}` : f.name;
    const n = rname ? `${rname}_${f.name}` : f.name;
    if (f.type === "avm_cdf_prob") {
      if (!f.dims.length || !/^CDF_SIZE\(/.test(f.dims.at(-1)!)) die(`${p} is not a CDF array`);
      leaves.push({ path: p, name: n, rank: f.dims.length });
    } else if (f.type === "int" && type === "FRAME_CONTEXT" && f.name === "initialized") {
      sawInitialized = true;
    } else if (structs[f.type]) {
      if (f.dims.length > 1) die(`${p}: multi-dimensional struct arrays are not handled`);
      if (f.dims.length === 1) {
        const k = Number(f.dims[0]);
        if (!Number.isInteger(k)) die(`${p}: struct array bound ${f.dims[0]} is not a literal`);
        for (let i = 0; i < k; i++) flatten(f.type, `${p}[${i}]`, `${n}${i}`);
      } else flatten(f.type, p, n);
    } else die(`${p}: field type ${f.type} is not a CDF`);
  }
}
flatten("FRAME_CONTEXT", "", "");
if (!sawInitialized) die("FRAME_CONTEXT has no trailing `int initialized`");

// get_q_ctx's buckets, read from the source.
const qctx = /static int get_q_ctx\(int q\) \{([\s\S]*?)\n\}/.exec(read("av2/common/entropy.c"))?.[1] ?? "";
const thresholds = [...qctx.matchAll(/if \(q <= (\d+)\) return (\d+);/g)].map((m) => [Number(m[1]), Number(m[2])]);
if (thresholds.length !== 3 || thresholds.some(([, r], i) => r !== i) || !/return 3;/.test(qctx))
  die("get_q_ctx is not the three-threshold function this generator expects");
const QINDEX_PER_BUCKET = [0, ...thresholds.map(([t]) => t + 1)];

// ---- 2 and 3. the probe ---------------------------------------------------
const work = fs.mkdtempSync(path.join(os.tmpdir(), "av2-cdf-gen-"));
const dim = (p: string, k: number) => `(int)(sizeof(fc_probe.${p}${"[0]".repeat(k)}) / sizeof(fc_probe.${p}${"[0]".repeat(k + 1)}))`;
const probe = `// generated by rust/gen/cdf.ts
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include "av2/common/av2_common_int.h"
#include "av2/common/entropy.h"
#include "av2/common/entropymv.h"
static FRAME_CONTEXT fc_probe;
int main(void) {
  printf("sizeof %zu %zu\\n", sizeof(FRAME_CONTEXT), offsetof(FRAME_CONTEXT, initialized));
${leaves
  .map(
    (l) =>
      `  printf("leaf ${l.name} %zu %zu${" %d".repeat(l.rank)}\\n", offsetof(FRAME_CONTEXT, ${l.path}), sizeof(fc_probe.${l.path})${Array.from({ length: l.rank }, (_, k) => `, ${dim(l.path, k)}`).join("")});`,
  )
  .join("\n")}
  static const int qindex[4] = { ${QINDEX_PER_BUCKET.join(", ")} };
  for (int q = 0; q < 4; q++) {
    for (int r = 0; r < 2; r++) {
      AV2_COMMON *cm = calloc(1, sizeof(*cm));
      cm->fc = calloc(1, sizeof(FRAME_CONTEXT));
      cm->default_frame_context = calloc(1, sizeof(FRAME_CONTEXT));
      cm->cur_frame = calloc(1, sizeof(*cm->cur_frame));
      if (!cm->fc || !cm->default_frame_context || !cm->cur_frame) return 1;
      cm->seq_params.reduced_tx_part_set = r;
      cm->quant_params.base_qindex = qindex[q];
      av2_setup_past_independence(cm);
      const uint16_t *v = (const uint16_t *)cm->default_frame_context;
      printf("variant %d %d %d", q, r, cm->default_frame_context->initialized);
      for (size_t i = 0; i < offsetof(FRAME_CONTEXT, initialized) / 2; i++) printf(" %u", v[i]);
      printf("\\n");
    }
  }
  return 0;
}
`;
fs.writeFileSync(path.join(work, "probe.c"), probe);
sh(["clang", "-O1", "-w", path.join(work, "probe.c"), path.join(AVM_BUILD, "libavm.a"), `-I${AVM_SRC}`, `-I${AVM_BUILD}`, "-o", path.join(work, "probe"), "-lm", "-lpthread"]);
const out = sh([path.join(work, "probe")]).split("\n").filter(Boolean);
fs.rmSync(work, { recursive: true, force: true });

const [, size, initOffset] = out[0].split(" ").map(Number);
if (initOffset + 4 !== size) die(`FRAME_CONTEXT has ${size - initOffset - 4} bytes after initialized`);
type Laid = Leaf & { offset: number; dims: number[]; nsyms: number; len: number };
const laid: Laid[] = out
  .filter((l) => l.startsWith("leaf "))
  .map((l, i) => {
    const [, name, off, bytes, ...d] = l.split(" ");
    const dims = d.map(Number);
    const last = dims.at(-1)!;
    if (last < 6) die(`${name}: CDF of ${last - 4} symbols`);
    return { ...leaves[i], name, offset: Number(off) / 2, len: Number(bytes) / 2, dims: dims.slice(0, -1), nsyms: last - 4 };
  });
let at = 0;
for (const l of laid) {
  if (l.offset !== at) die(`${l.name} sits at ${l.offset}, expected ${at}: FRAME_CONTEXT has a gap`);
  if (l.len !== l.dims.reduce((a, b) => a * b, 1) * (l.nsyms + 4)) die(`${l.name}: size and dims disagree`);
  at += l.len;
}
// Only alignment padding may sit between the last CDF and `initialized`.
const padding = initOffset - at * 2;
if (padding < 0 || padding > 3) die(`the CDFs cover ${at * 2} bytes; initialized sits at ${initOffset}`);
const FC_LEN = at;

const variants: number[][][] = [0, 1, 2, 3].map(() => []);
for (const l of out.filter((l) => l.startsWith("variant "))) {
  const [, q, r, init, ...vals] = l.split(" ").map(Number);
  if (init !== 1) die(`variant ${q}/${r}: initialized is ${init}`);
  // The probe prints up to initialized; anything past FC_LEN is padding.
  if (vals.length !== Math.floor(initOffset / 2)) die(`variant ${q}/${r}: ${vals.length} values`);
  variants[q][r] = vals.slice(0, FC_LEN);
}
const base = variants[0][0];

// Each CDF's alphabet ends at its first 0 (AVM_ICDF(CDF_PROB_TOP)). Most
// fields use the full declared alphabet; a few pack smaller ones into the same
// array (palette colors by palette size, say) and leave the tail zero. Every
// CDF must have its 0 within the declared alphabet in every variant, or the
// layout reading above is wrong.
const alphabets = new Map<string, Set<number>>();
for (const l of laid) {
  const stride = l.nsyms + 4;
  const seen = new Set<number>();
  for (const v of variants.flat()) {
    for (let c = l.offset; c < l.offset + l.len; c += stride) {
      const end = v.slice(c, c + l.nsyms).indexOf(0);
      // A CDF starting at 0 is a one-symbol alphabet: an entry AVM never reads
      // (intra_ext_tx_cdf's DCT-only set, say), zero in AVM's own table.
      if (end < 0) die(`${l.name}: the CDF at ${c} never reaches 0`);
      seen.add(end + 1);
    }
  }
  alphabets.set(l.name, seen);
}
const packed = laid.filter((l) => alphabets.get(l.name)!.size > 1 || !alphabets.get(l.name)!.has(l.nsyms));

const differs = (a: number[], b: number[], l: Laid) => a.slice(l.offset, l.offset + l.len).some((x, i) => x !== b[l.offset + i]);
const byQ = laid.filter((l) => [1, 2, 3].some((q) => differs(variants[q][0], base, l)));
const byReduced = laid.filter((l) => differs(variants[0][1], base, l));
for (const l of laid) {
  const q = byQ.includes(l), r = byReduced.includes(l);
  if (q && r) die(`${l.name} depends on both the q bucket and reduced_tx_part_set`);
  for (let qq = 0; qq < 4; qq++) {
    // The two inputs must act independently: q never changes a reduced field
    // and the reduced flag never changes a q field.
    if (differs(variants[qq][1], variants[qq][0], l) !== r) die(`${l.name}: reduced_tx_part_set acts differently at q bucket ${qq}`);
    if (differs(variants[qq][0], base, l) && !q) die(`${l.name}: unexpected q dependence`);
  }
}

// ---- emit -----------------------------------------------------------------
const rows = (v: number[], indent: string) => {
  const lines: string[] = [];
  for (let i = 0; i < v.length; i += 16) lines.push(indent + v.slice(i, i + 16).join(", ") + ",");
  return lines.join("\n");
};
const slice = (v: number[], l: Laid) => v.slice(l.offset, l.offset + l.len);
const cname = (l: Laid) => l.name.toUpperCase();

let rs = `// @generated by rust/gen/cdf.ts from AVM v1.0.0 (${AVM_COMMIT}). Do not edit;
// rerun the generator. The values are AVM's own av2_setup_past_independence()
// output, the layout is FRAME_CONTEXT's (av2/common/entropymode.h).

use crate::cdf::Cdf;

/// The AVM commit these tables came from.
pub const AVM_COMMIT: &str = "${AVM_COMMIT}";
/// u16 values in FRAME_CONTEXT, all of them CDFs, in AVM's memory order.
pub const FC_LEN: usize = ${FC_LEN};
/// sizeof(FRAME_CONTEXT): the CDFs, alignment padding, then \`int initialized\`.
pub const AVM_FRAME_CONTEXT_SIZE: usize = ${size};
/// Byte offset of \`initialized\`; the bytes between the CDFs and it are padding.
pub const AVM_INITIALIZED_OFFSET: usize = ${initOffset};
/// get_q_ctx (av2/common/entropy.c): base_qindex at or below each threshold
/// picks that coefficient-CDF bucket; above the last, bucket 3.
pub const QCTX_THRESHOLDS: [u32; 3] = [${thresholds.map(([t]) => t).join(", ")}];

`;
for (const l of laid) {
  rs += `pub const ${cname(l)}: Cdf<${l.dims.length}> = Cdf::new("${l.name}", ${l.offset}, [${l.dims.join(", ")}], ${l.nsyms});\n`;
}
rs += `\n/// Fields whose CDFs don't all use the declared alphabet: smaller alphabets
/// share the array and end early, and size 1 marks an entry AVM never reads.
/// (name, alphabet sizes found).
pub const PACKED_ALPHABETS: [(&str, &[usize]); ${packed.length}] = [
${packed.map((l) => `    ("${l.name}", &[${[...alphabets.get(l.name)!].sort((a, b) => a - b).join(", ")}]),`).join("\n")}
];

/// Every field: (name, offset, length), in memory order.
pub const FIELDS: [(&str, usize, usize); ${laid.length}] = [
${laid.map((l) => `    ("${l.name}", ${l.offset}, ${l.len}),`).join("\n")}
];

/// The defaults at q bucket 0 with reduced_tx_part_set off.
pub static DEFAULT: [u16; FC_LEN] = [
${laid.map((l) => `    // ${l.name}${l.dims.map((d) => `[${d}]`).join("")}, ${l.nsyms} symbols\n${rows(slice(base, l), "    ")}`).join("\n")}
];

/// Fields av2_default_coef_probs sets by q bucket: (offset, values per bucket).
pub static QCTX_FIELDS: [(usize, [&[u16]; 4]); ${byQ.length}] = [
${byQ
  .map(
    (l) => `    // ${l.name}\n    (${l.offset}, [\n${[0, 1, 2, 3].map((q) => `        &[\n${rows(slice(variants[q][0], l), "            ")}\n        ],`).join("\n")}\n    ]),`,
  )
  .join("\n")}
];

/// Fields init_mode_probs replaces when reduced_tx_part_set is on.
pub static REDUCED_TX_PART_SET_FIELDS: [(usize, &[u16]); ${byReduced.length}] = [
${byReduced.map((l) => `    // ${l.name}\n    (${l.offset}, &[\n${rows(slice(variants[0][1], l), "        ")}\n    ]),`).join("\n")}
];
`;
fs.writeFileSync(OUT, rs);
console.log(
  `wrote ${path.relative(process.cwd(), OUT)}: ${laid.length} CDF fields, ${FC_LEN} values, ${byQ.length} by q bucket, ${byReduced.length} by reduced_tx_part_set`,
);
