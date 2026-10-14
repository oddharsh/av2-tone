// simde.ts — give an arm64 AVM build the x86 SIMD kernels, through SIMDe.
//
// WHY. AVM (the AV2 reference encoder) ships an AVX2 version of nearly every hot
// kernel and almost no NEON: on arm64 it has four NEON files. Profiled on an M3
// Max, a single-threaded 600px encode spent at least 34% of its samples in plain
// C kernels (transforms, directional prediction, quantizers) and 1.3% in NEON.
// SIMDe (github.com/simd-everywhere/simde) implements the x86 intrinsics on
// NEON, so AVM's own x86 files compile for arm64 unchanged.
//
// WHAT IT DOES, given a libavif build tree that has already been built once:
//   1. generates AVM's rtcd dispatch headers for x86_64 with every ISA up to
//      AVX2 required, beside the arm64 ones ninja generated;
//   2. compiles every x86 kernel file through a shim that maps <immintrin.h>
//      and friends onto SIMDe;
//   3. merges the dispatch: wherever arm64 falls back to plain C and a compiled
//      x86 variant exists, dispatch to it (NEON choices are kept), iterating
//      until the object set is closed, since a few kernels call NASM-only
//      routines that no C file defines;
//   4. swaps the merged headers into the build, rebuilds, appends the objects to
//      libavm.a and relinks avifenc/avifdec.
//
// THE BAR IS BYTE IDENTITY. The kernels are integer and bit-exact with their C
// versions by AVM's own unit tests, so the output must not move by one byte.
// build.sh checks that against the pristine build before it reports success.
// Measured 2026-09-27: 20 of 20 encodes identical across 4 inputs and 5 option
// sets, every decode identical, and 1.35x on a 600px encode (1.54x at 2400px,
// about 1.27x decode). 539 dispatch entries move; 2 files are dropped because
// they call NASM, and CDEF and disflow do not translate.
//
// FOUR THINGS THAT COST TIME, so nobody re-derives them:
//   - AVM passes shift counts at run time, and SIMDe's NEON macro forms of the
//     immediate shifts need constants; the shim undefines those macros so the
//     function forms are used, and SIMDE_NO_CHECK_IMMEDIATE_CONSTANT turns off
//     the diagnose_if guard that would otherwise reject the same calls.
//   - ninja GENERATES the rtcd headers during the build, so a swap made before
//     the first build is silently overwritten. Swap after, and touch them.
//   - x86 files call sibling variants directly (an _avx2 calling an _sse4_1), so
//     the headers used to compile them carry every x86 prototype, placed after
//     the real declarations because they need AVM's types.
//   - `grep -q` against `nm` under pipefail reports a symbol missing when it is
//     present (SIGPIPE), which made a gate refuse a good build. Count instead.
//
// usage: bun simde.ts --src <patched AVM source> --build <libavif build dir>
//                     --simde <simde checkout> --work <scratch dir>
import fs from "node:fs";
import path from "node:path";

const arg = (name: string): string => {
  const i = process.argv.indexOf(`--${name}`);
  const v = i >= 0 ? process.argv[i + 1] : undefined;
  if (!v) throw new Error(`--${name} is required`);
  return path.resolve(v);
};
const SRC = arg("src"), BUILD = arg("build"), SIMDE = arg("simde"), WORK = arg("work");
const CFG = path.join(BUILD, "_deps/libavm-build/config");
const LIB = path.join(BUILD, "_deps/libavm-build/libavm.a");
const HEADERS = ["av2_rtcd", "avm_dsp_rtcd", "avm_scale_rtcd"] as const;
const DEFS: Record<(typeof HEADERS)[number], string> = {
  av2_rtcd: "av2/common/av2_rtcd_defs.pl",
  avm_dsp_rtcd: "avm_dsp/avm_dsp_rtcd_defs.pl",
  avm_scale_rtcd: "avm_scale/avm_scale_rtcd.pl",
};
const ISA = "(?:mmx|sse|sse2|sse3|ssse3|sse4_1|sse4_2|avx|avx2)";
const MARK = "/* simde */";

function run(cmd: string[], opts: { cwd?: string; allowFail?: boolean } = {}): string {
  const r = Bun.spawnSync(cmd, { cwd: opts.cwd, stdout: "pipe", stderr: "pipe" });
  if (r.exitCode !== 0 && !opts.allowFail) {
    throw new Error(`${cmd.slice(0, 3).join(" ")} failed (${r.exitCode}):\n${r.stderr.toString().slice(-2000)}`);
  }
  return r.stdout.toString();
}
const write = (p: string, s: string) => { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, s); };

for (const p of [SRC, CFG, LIB, path.join(SIMDE, "simde/x86/avx2.h")]) {
  if (!fs.existsSync(p)) throw new Error(`missing ${p}: build the tree once first, and check out SIMDe`);
}
fs.mkdirSync(WORK, { recursive: true });

// 1. the arm64 headers as ninja generated them (kept, since the swap replaces them)
const armDir = path.join(WORK, "rtcd-arm64");
for (const h of HEADERS) {
  const live = fs.readFileSync(path.join(CFG, `${h}.h`), "utf8");
  const kept = path.join(armDir, `${h}.h`);
  if (!live.includes(MARK)) write(kept, live);
  else if (!fs.existsSync(kept)) throw new Error(`${h}.h is already merged and no arm64 copy was kept; rebuild with --force`);
}
const x86Dir = path.join(WORK, "rtcd-x86");
const requires = ["mmx", "sse", "sse2", "sse3", "ssse3", "sse4_1", "sse4_2", "avx", "avx2"].map((i) => `--require-${i}`);
for (const h of HEADERS) {
  write(path.join(x86Dir, `${h}.h`), run(["perl", path.join(SRC, "cmake/rtcd.pl"), "--arch=x86_64", `--sym=${h}`, ...requires,
    `--config=${path.join(CFG, "avm_config.h")}`, path.join(SRC, DEFS[h])]));
}

// 2. the shim: every x86 intrinsic header maps onto SIMDe
const shim = path.join(WORK, "shim");
const undef = ["mm", "mm256"].flatMap((p) => ["slli", "srli", "srai"].flatMap((s) => ["epi16", "epi32", "epi64"].map((t) => `#undef simde_${p}_${s}_${t}`)));
const shimBody = [
  "#pragma once", "#define SIMDE_ENABLE_NATIVE_ALIASES",
  "#include <simde/x86/avx2.h>", "#include <simde/x86/sse4.2.h>", "#include <simde/x86/fma.h>",
  "/* AVM passes shift counts at run time; the NEON macro forms need constants. */",
  ...undef,
  "#define _mm256_zeroupper() ((void)0)",
  "#undef _mm256_i32gather_epi32",
  "#define _mm256_i32gather_epi32(b, i, s) simde_mm256_i32gather_epi32((const int32_t *)(b), (i), (s))",
  "",
].join("\n");
for (const h of ["immintrin", "emmintrin", "xmmintrin", "pmmintrin", "tmmintrin", "smmintrin", "nmmintrin", "x86intrin", "mmintrin", "wmmintrin"]) {
  write(path.join(shim, `${h}.h`), shimBody);
}
// the config the x86 files see: same build, with the x86 ISAs switched on
const cfgX86 = path.join(WORK, "cfg-x86/config");
write(path.join(cfgX86, "avm_config.h"), fs.readFileSync(path.join(CFG, "avm_config.h"), "utf8")
  .replace(/^#define (HAVE_(?:MMX|SSE|SSE2|SSE3|SSSE3|SSE4_1|SSE4_2|AVX|AVX2)) 0$/gm, "#define $1 1"));

// every x86 prototype, per header, for the objects' own direct calls
const protos = (h: string): string => {
  const text = fs.readFileSync(path.join(x86Dir, `${h}.h`), "utf8").split("\n")
    .filter((l) => !l.trimStart().startsWith("#")).join("\n");
  const out = new Set<string>();
  const decl = new RegExp(`^[\\w\\s*]+\\b\\w+_${ISA}\\s*\\(`);
  for (const stmt of text.split(";")) {
    const s = stmt.replace(/\/\*[\s\S]*?\*\//g, "").trim();
    if (/[{}]|\benum\b|\btypedef\b|RTCD_EXTERN/.test(s)) continue;
    if (decl.test(s)) out.add(`${s};`);
  }
  return [...out].join("\n");
};

// 3. merge, compile, close, until the object set stops changing
function merge(defined: Set<string>): { headers: Record<string, string>; moved: number } {
  const headers: Record<string, string> = {};
  let moved = 0;
  for (const h of HEADERS) {
    const arm = fs.readFileSync(path.join(armDir, `${h}.h`), "utf8");
    const x86 = fs.readFileSync(path.join(x86Dir, `${h}.h`), "utf8");
    const choice = new Map<string, string>();
    for (const m of x86.matchAll(/^#define (\w+) (\w+)$/gm)) if (m[2] !== `${m[1]}_c`) choice.set(m[1], m[2]);
    const proto = (v: string): string | null => {
      const i = x86.search(new RegExp(`\\b${v}\\(`));
      return i < 0 ? null : x86.slice(x86.lastIndexOf("\n", i) + 1, x86.indexOf(";", i) + 1);
    };
    headers[h] = arm.replace(/^#define (\w+) (\w+)_c$/gm, (line: string, fn: string) => {
      const v = choice.get(fn);
      const p = v && defined.has(v) ? proto(v) : null;
      if (!v || !p) return line;
      moved++;
      return `${p}\n#define ${fn} ${v}  ${MARK}`;
    });
  }
  return { headers, moved };
}
const kernelSources = [
  ...["av2/common/x86", "avm_dsp/x86", "av2/encoder/x86"].flatMap((d) =>
    fs.readdirSync(path.join(SRC, d)).filter((f) => f.endsWith(".c") && f !== "avm_asm_stubs.c").map((f) => `${d}/${f}`)),
  ...fs.readdirSync(path.join(SRC, "avm_dsp/flow_estimation/x86")).filter((f) => f.startsWith("corner_match_") && f.endsWith(".c"))
    .map((f) => `avm_dsp/flow_estimation/x86/${f}`),
  "av2/common/gdf_block_avx2.c",
];
const objDir = path.join(WORK, "obj");
function compile(): string[] {
  fs.rmSync(objDir, { recursive: true, force: true });
  fs.mkdirSync(objDir, { recursive: true });
  const failed: string[] = [];
  for (const f of kernelSources) {
    const o = path.join(objDir, `${f.replaceAll("/", "_").replace(/\.c$/, "")}.o`);
    const r = Bun.spawnSync(["clang", "-c", "-O3", "-std=gnu11", "-fPIC", "-DNDEBUG", "-D_FILE_OFFSET_BITS=64",
      "-DSIMDE_NO_CHECK_IMMEDIATE_CONSTANT", "-w", `-I${shim}`, `-I${SIMDE}`, `-I${path.dirname(cfgX86)}`,
      `-I${path.join(BUILD, "_deps/libavm-build")}`, `-I${SRC}`, `-I${path.join(SRC, "third_party/tensorflow")}`,
      path.join(SRC, f), "-o", o], { stdout: "ignore", stderr: "ignore" });
    if (r.exitCode !== 0) failed.push(f);
  }
  return failed;
}
type Syms = { def: Set<string>; und: Set<string> };
function symbols(file: string, definedOnly = false): Syms {
  const def = new Set<string>(), und = new Set<string>();
  for (const line of run(["nm", ...(definedOnly ? ["-g", "--defined-only"] : []), file], { allowFail: true }).split("\n")) {
    const m = line.match(/^\s*(?:[0-9a-f]+)?\s+([A-Za-z])\s+(\S+)$/);
    if (!m) continue;
    if (m[1] === "U") und.add(m[2]);
    else if (/[TDBSC]/.test(m[1])) def.add(m[2]);
  }
  return { def, und };
}
// a symbol the objects may leave to the plain library or the system
const ours = (s: string) => new RegExp(`_${ISA}$`).test(s) || /^_(avm|av2)_/.test(s);
function close(base: Set<string>): { kept: string[]; defined: Set<string> } {
  const objs = fs.readdirSync(objDir).filter((f) => f.endsWith(".o"));
  const syms = new Map(objs.map((o) => [o, symbols(path.join(objDir, o))]));
  const kept = new Set(objs);
  for (;;) {
    const have = new Set(base);
    for (const o of kept) for (const s of syms.get(o)!.def) have.add(s);
    const drop = [...kept].filter((o) => [...syms.get(o)!.und].some((s) => ours(s) && !have.has(s)));
    if (!drop.length) break;
    for (const o of drop) kept.delete(o);
  }
  const defined = new Set([...kept].flatMap((o) => [...syms.get(o)!.def]).map((s) => s.replace(/^_/, "")));
  return { kept: [...kept].sort(), defined };
}

// libavm.a exactly as the build made it, before anything is appended
const baseSyms = symbols(LIB, true).def;
let defined = new Set<string>();
{
  // round 0 compiles against the x86 dispatch to learn what translates at all
  for (const h of HEADERS) write(path.join(cfgX86, `${h}.h`), fs.readFileSync(path.join(x86Dir, `${h}.h`), "utf8"));
  const failed = compile();
  if (failed.length) console.error(`not translatable through SIMDe (C stays): ${failed.map((f) => path.basename(f)).join(", ")}`);
  defined = close(baseSyms).defined;
}
let kept: string[] = [];
let merged: Record<string, string> = {};
for (let round = 1; ; round++) {
  const m = merge(defined);
  merged = m.headers;
  for (const h of HEADERS) {
    const withProtos = merged[h].replace(new RegExp(`(void ${h}\\(void\\);\\n)`),
      `$1\n/* every x86 variant, for the SIMDe-compiled objects only */\n${protos(h)}\n`);
    write(path.join(cfgX86, `${h}.h`), withProtos);
  }
  const failed = compile();
  if (failed.length) throw new Error(`failed against the merged dispatch: ${failed.join(", ")}`);
  const c = close(baseSyms);
  kept = c.kept;
  const stable = c.defined.size === defined.size && [...c.defined].every((s) => defined.has(s));
  console.error(`round ${round}: ${m.moved} dispatch entries moved, ${kept.length} objects kept`);
  defined = c.defined;
  if (stable) break;
  if (round >= 5) throw new Error("the SIMDe object set did not converge in 5 rounds");
}

// 4. swap, rebuild, append, relink
for (const h of HEADERS) {
  write(path.join(CFG, `${h}.h`), merged[h]);
  const now = new Date();
  fs.utimesSync(path.join(CFG, `${h}.h`), now, now);
}
const build = () => Bun.spawnSync(["cmake", "--build", BUILD, "--target", "avifenc", "avifdec"], { stdout: "pipe", stderr: "pipe" });
const first = build().stdout.toString();
if (/Generating .*_rtcd\.h/.test(first)) throw new Error("ninja regenerated the rtcd headers over the merged ones");
run(["ar", "rs", LIB, ...kept.map((o) => path.join(objDir, o))]);
const now = new Date();
fs.utimesSync(LIB, now, now);
const second = build();
if (second.exitCode !== 0) throw new Error(`relink failed:\n${second.stdout.toString().slice(-2000)}`);
const linked = run(["nm", path.join(BUILD, "avifenc")]).split("\n").filter((l) => l.endsWith(" _fwd_txfm_avx2")).length;
if (linked < 1) throw new Error("avifenc linked without the SIMDe kernels");
console.error(`SIMDe kernels linked: ${kept.length} objects`);
