// defs.ts: writes av2-tone/src/avm_defs.rs, AVM's enum constants and lookup
// tables, out of AVM's own headers. Nothing in the output is typed by hand:
//
//   1. clang's preprocessor runs over the headers the decoder includes, so
//      only enums that survive AVM's #if configuration are seen. Every
//      enumerator in an AVM header becomes a Rust constant; a C probe prints
//      its value, so `BLOCK_INVALID = BLOCK_SIZES_ALL` and the like are
//      evaluated by the compiler, not here.
//   2. Each table in TABLES below is printed by the same probe, its
//      dimensions taken from sizeof. The list says which tables to export,
//      never what is in them.
//
// Refuses an AVM tree that is dirty or not at v1.0.0, as ../gen/cdf.ts does.
//
//   rust/oracle/build.sh && bun rust/gen/defs.ts
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const AVM_COMMIT = "966a7d7cd6fcf60360caf5dc413b2aeeb65e144d";
const HERE = import.meta.dir;
const AVM_SRC = path.resolve(process.env.AVM_SRC ?? path.join(HERE, "../../encoder/build/_deps/libavm-src"));
const AVM_BUILD = path.join(HERE, "../build/avm");
const OUT = path.join(HERE, "../av2-tone/src/avm_defs.rs");

// Headers the probe includes; enums are taken from any AVM header they pull in.
const INCLUDES = [
  "av2/common/av2_common_int.h",
  "av2/common/blockd.h",
  "av2/common/common_data.h",
  "av2/common/entropy.h",
  "av2/common/entropymode.h",
  "av2/common/entropymv.h",
  "av2/common/scan.h",
  "av2/common/txb_common.h",
  "av2/common/pred_common.h",
  "av2/common/cfl.h",
  "av2/common/reconintra.h",
  "av2/common/mvref_common.h",
  "av2/common/quant_common.h",
  "av2/common/seg_common.h",
  "av2/common/cdef_block.h",
  "av2/common/ccso.h",
  "av2/common/gdf.h",
  "av2/common/secondary_tx.h",
];

// #define constants to evaluate (the preprocessor erases macros, so they are
// listed by name; the probe gives their values).
const DEFINES = [
  "MI_SIZE_LOG2", "MI_SIZE", "MAX_MIB_SIZE_LOG2", "MAX_MIB_SIZE", "MAX_MIB_MASK", "MAX_SB_SIZE_LOG2",
  "MAX_SEGMENTS", "MAX_SEGMENTS_8", "INTRA_FRAME", "NONE_FRAME", "PARTITION_PLOFFSET", "PARTITION_CONTEXTS",
  "MAX_TX_SIZE_LOG2", "MAX_TX_SQUARE", "TX_PAD_HOR", "TX_PAD_HOR_LOG2", "TX_PAD_TOP", "TX_PAD_BOTTOM",
  "PALETTE_MAX_SIZE", "PALETTE_MIN_SIZE", "PALETTE_COLORS",
  "CDEF_STRENGTHS_NUM", "CDEF_SB_SHIFT", "BR_CDF_SIZE", "COEFF_BASE_RANGE", "NUM_BASE_LEVELS",
  "MAX_BASE_BR_RANGE", "LF_NUM_BASE_LEVELS", "LF_MAX_BASE_BR_RANGE", "LF_BASE_SYMBOLS",
  "MAX_NUM_NEIGHBORS", "NON_DIRECTIONAL_MODES_COUNT", "TOTAL_ANGLE_DELTA_COUNT", "MAX_ANGLE_DELTA",
  "LUMA_MODE_COUNT", "FIRST_MODE_COUNT", "SECOND_MODE_COUNT", "LUMA_INTRA_MODE_INDEX_COUNT",
  "LUMA_INTRA_MODE_OFFSET_COUNT", "CHROMA_INTRA_MODE_INDEX_COUNT", "INTRA_MODE_SETS",
  "FSC_MAXWIDTH", "FSC_MAXHEIGHT", "FSC_MINWIDTH", "FSC_MINHEIGHT", "SKIP_CONTEXTS", "MRL_LINE_NUMBER",
  "MHCCP_MODE_NUM", "CFL_MHCCP_SWITCH_NUM", "PALETTE_SIZES",
  "CFL_JOINT_SIGNS", "CFL_ALPHABET_SIZE", "CFL_ALPHABET_SIZE_LOG2", "CFL_BUF_LINE",
  "MI_IN_CDEF_LINEAR_LOG2", "CDEF_IN_SB_STRIDE", "MIN_SB_SIZE_LOG2", "MAX_NUM_SHELL_CLASS",
  "MAX_COL_TRUNCATED_UNARY_VAL", "NUM_CTX_COL_MV_GTX", "NUM_CTX_COL_MV_INDEX", "NUM_ALLOWED_BV_PRECISIONS",
  "CCSO_BLK_SIZE", "MV_LOW", "MV_UPP",
  "TX_PAD_LEFT", "TX_PAD_2D", "COEFF_CONTEXT_BITS", "COEFF_CONTEXT_MASK", "LF_2D_LIM", "LF_2D_LIM_UV", "LF_RC_LIM",
  "LF_RC_LIM_UV", "LF_SIG_COEF_CONTEXTS_2D", "LF_SIG_COEF_CONTEXTS_2D_UV", "COEFF_BASE_PH_CONTEXTS", "PHTHRESH",
  "V_TXB_SKIP_CONTEXT_OFFSET", "TXB_SKIP_CONTEXTS", "EOB_MAX_SYMS", "EOB_PT_INDEX_COUNT", "IST_SET_SIZE",
  "IST_REDUCED_SET_SIZE", "IST_4x4_HEIGHT", "IST_8x8_HEIGHT_RED", "IST_8x8_HEIGHT", "IST_ADST_NZ_CNT", "PRIMARY_TX_BITS",
  "SECONDARY_TX_BITS", "INTER_TX_TYPE_INDEX_COUNT", "INTER_TX_TYPE_OFFSET1_COUNT", "INTER_TX_TYPE_OFFSET2_COUNT",
  "STX_TYPES", "WAIP_WH_RATIO_2_THRES", "WAIP_WH_RATIO_4_THRES", "WAIP_WH_RATIO_8_THRES", "WAIP_WH_RATIO_16_THRES",
  "ANGLE_STEP", "MAX_VAL_BR_CTX", "TCQ_MAX_STATES", "EXT_TX_SETS_INTRA", "EXT_TX_SIZES", "INTRA_TX_SET1",
];

// [Rust name, C expression]. Plain arrays get their shape from sizeof; an
// array of structs is listed once per field, as `table[*].field`.
const TABLES: [string, string][] = [
  ["MI_SIZE_WIDE_LOG2", "mi_size_wide_log2"],
  ["MI_SIZE_HIGH_LOG2", "mi_size_high_log2"],
  ["MI_SIZE_WIDE", "mi_size_wide"],
  ["MI_SIZE_HIGH", "mi_size_high"],
  ["BLOCK_SIZE_WIDE", "block_size_wide"],
  ["BLOCK_SIZE_HIGH", "block_size_high"],
  ["SIZE_GROUP_LOOKUP", "size_group_lookup"],
  ["FSC_BSIZE_GROUPS", "fsc_bsize_groups"],
  ["NUM_PELS_LOG2_LOOKUP", "num_pels_log2_lookup"],
  ["SUBSIZE_LOOKUP", "subsize_lookup"],
  ["MAX_TXSIZE_LOOKUP", "max_txsize_lookup"],
  ["MAX_TXSIZE_RECT_LOOKUP", "max_txsize_rect_lookup"],
  ["TX_SIZE_WIDE", "tx_size_wide"],
  ["TX_SIZE_HIGH", "tx_size_high"],
  ["TX_SIZE_WIDE_UNIT", "tx_size_wide_unit"],
  ["TX_SIZE_HIGH_UNIT", "tx_size_high_unit"],
  ["TX_SIZE_WIDE_LOG2", "tx_size_wide_log2"],
  ["TX_SIZE_HIGH_LOG2", "tx_size_high_log2"],
  ["TX_SIZE_2D", "tx_size_2d"],
  ["TXSIZE_SQR_MAP", "txsize_sqr_map"],
  ["TXSIZE_SQR_UP_MAP", "txsize_sqr_up_map"],
  ["TXSIZE_LOG2_MINUS4", "txsize_log2_minus4"],
  ["TX_MODE_TO_BIGGEST_TX_SIZE", "tx_mode_to_biggest_tx_size"],
  ["SS_SIZE_LOOKUP", "ss_size_lookup"],
  ["PARTITION_CONTEXT_ABOVE", "partition_context_lookup[*].above"],
  ["PARTITION_CONTEXT_LEFT", "partition_context_lookup[*].left"],
  ["AV2_MD_CLASS", "av2_md_class"],
  ["AV2_SIZE_CLASS", "av2_size_class"],
  ["AV2_EXT_TX_IND", "av2_ext_tx_ind"],
  ["AV2_EXT_TX_INV", "av2_ext_tx_inv"],
  ["AV2_MD_TYPE2IDX", "av2_md_type2idx"],
  ["VTX_TAB", "vtx_tab"],
  ["HTX_TAB", "htx_tab"],
  ["REORDERED_Y_MODE", "reordered_y_mode"],
  ["DEFAULT_MODE_LIST_Y", "default_mode_list_y"],
  ["DEFAULT_MODE_LIST_UV", "default_mode_list_uv"],
  ["AV2_MD_IDX2TYPE", "av2_md_idx2type"],
  ["AV2_NUM_EXT_TX_SET", "av2_num_ext_tx_set"],
  ["AV2_NUM_EXT_TX_SET_INTRA", "av2_num_ext_tx_set_intra"],
  ["AV2_NUM_REDUCED_TX_SET", "av2_num_reduced_tx_set"],
  ["AV2_EXT_TX_USED", "av2_ext_tx_used"],
  ["AV2_EXT_TX_USED_FLAG", "av2_ext_tx_used_flag"],
  ["EXT_TX_SET_INDEX", "ext_tx_set_index"],
  ["EXT_INDEX_TO_TX_TYPE_LARGE_TXFM", "ext_index_to_tx_type_large_txfm"],
  ["INV_MOST_PROBABLE_STX_MAPPING", "inv_most_probable_stx_mapping"],
  ["INV_MOST_PROBABLE_STX_MAPPING_ADST_ADST", "inv_most_probable_stx_mapping_ADST_ADST"],
  ["PH_ALLOWED_TX_TYPES", "ph_allowed_tx_types"],
  ["TX_TYPE_TO_CLASS", "tx_type_to_class"],
  ["MODE_TO_ANGLE_MAP", "mode_to_angle_map"],
  ["AV2_EOB_GROUP_START", "av2_eob_group_start"],
  ["AV2_EOB_OFFSET_BITS", "av2_eob_offset_bits"],
];

function sh(cmd: string[], cwd?: string): string {
  const p = Bun.spawnSync(cmd, { cwd, stdout: "pipe", stderr: "pipe" });
  if (p.exitCode !== 0) throw new Error(`${cmd.join(" ")}:\n${p.stderr.toString().slice(-3000)}`);
  return p.stdout.toString();
}
function die(msg: string): never {
  console.error(`error: ${msg}`);
  process.exit(1);
}

if (!fs.existsSync(path.join(AVM_SRC, "av2/common/enums.h"))) die(`no AVM source at ${AVM_SRC}; set AVM_SRC`);
const head = sh(["git", "-C", AVM_SRC, "rev-parse", "HEAD"]).trim();
if (head !== AVM_COMMIT) die(`${AVM_SRC} is at ${head}, not AVM v1.0.0 (${AVM_COMMIT})`);
if (sh(["git", "-C", AVM_SRC, "status", "--porcelain", "--untracked-files=no"]).trim())
  die(`${AVM_SRC} has local changes; the definitions must come from pristine AVM`);
if (!fs.existsSync(path.join(AVM_BUILD, "libavm.a"))) die(`no ${AVM_BUILD}/libavm.a: run rust/oracle/build.sh`);

const work = fs.mkdtempSync(path.join(os.tmpdir(), "av2-defs-gen-"));
const cc = ["clang", "-w", `-I${AVM_SRC}`, `-I${AVM_BUILD}`];
const includes = INCLUDES.map((h) => `#include "${h}"`).join("\n");

// ---- 1. the enums that survive the preprocessor ---------------------------
fs.writeFileSync(path.join(work, "pre.c"), `${includes}\n`);
const pre = sh([...cc, "-E", path.join(work, "pre.c")]);
const names: { name: string; file: string }[] = [];
const seen = new Set<string>();
let file = "";
let buf = "";
const flushEnums = () => {
  // `typedef enum ATTRIBUTE_PACKED {` expands to an attribute, so allow
  // anything but a brace or semicolon between `enum` and its body.
  for (const m of buf.matchAll(/\benum\b[^{};]*\{([^{}]*)\}/g)) {
    for (const item of m[1].split(",")) {
      const name = /^\s*([A-Za-z_]\w*)/.exec(item)?.[1];
      if (name && !seen.has(name)) {
        seen.add(name);
        names.push({ name, file });
      }
    }
  }
  buf = "";
};
for (const line of pre.split("\n")) {
  const marker = /^# \d+ "([^"]+)"/.exec(line);
  if (marker) {
    flushEnums();
    file = marker[1].startsWith(AVM_SRC) ? path.relative(AVM_SRC, marker[1]) : "";
    continue;
  }
  if (file) buf += line + "\n";
}
flushEnums();
if (names.length < 100) die(`only ${names.length} enum constants found; the header parse is wrong`);

// ---- 2. the probe ---------------------------------------------------------
// A table's rank comes from its declaration in the preprocessed source; each
// dimension then comes from sizeof, so the compiler evaluates the bounds.
type Table = { rust: string; c: string; field?: string; rank: number };
const tables: Table[] = TABLES.map(([rust, expr]) => {
  const m = /^(\w+)\[\*\]\.(\w+)$/.exec(expr);
  const c = m ? m[1] : expr;
  const decl = new RegExp(`\\b${c}\\s*((?:\\[[^\\]]*\\]\\s*)+)[=;]`).exec(pre);
  if (!decl) die(`no declaration of ${c} in the preprocessed headers`);
  const rank = (decl[1].match(/\[/g) ?? []).length;
  if (m && rank !== 1) die(`${c}: struct tables must be one-dimensional`);
  return { rust, c, field: m?.[2], rank };
});
const idx = (k: number) => "[0]".repeat(k);
const probe = `// generated by rust/gen/defs.ts
#include <stdio.h>
#include <stddef.h>
${includes}
int main(void) {
${names.map((n) => `  printf("enum ${n.name} %lld\\n", (long long)(${n.name}));`).join("\n")}
${DEFINES.map((d) => `#ifdef ${d}\n  printf("define ${d} %lld\\n", (long long)(${d}));\n#else\n  printf("missing ${d}\\n");\n#endif`).join("\n")}
${tables
  .map((t) => {
    if (t.field)
      return `  printf("table ${t.rust} 1 %zu", sizeof(${t.c}) / sizeof(${t.c}[0]));
  for (size_t i = 0; i < sizeof(${t.c}) / sizeof(${t.c}[0]); i++) printf(" %lld", (long long)${t.c}[i].${t.field});
  printf("\\n");`;
    const dims = Array.from({ length: t.rank }, (_, k) => `sizeof(${t.c}${idx(k)}) / sizeof(${t.c}${idx(k + 1)})`);
    return `  printf("table ${t.rust} ${t.rank}${" %zu".repeat(t.rank)}", ${dims.join(", ")});
  for (size_t i = 0; i < sizeof(${t.c}) / sizeof(${t.c}${idx(t.rank)}); i++)
    printf(" %lld", (long long)((const __typeof__(${t.c}${idx(t.rank)}) *)${t.c})[i]);
  printf("\\n");`;
  })
  .join("\n")}
  // av2_scan_orders: each [tx_size][tx_type] scan, as long as the largest eob.
  for (int s = 0; s < TX_SIZES_ALL; s++)
    for (int t = 0; t < TX_TYPES; t++) {
      const int16_t *scan = av2_scan_orders[s][t].scan;
      const int n = scan ? av2_get_max_eob(s) : 0;
      printf("scan %d %d %d", s, t, n);
      for (int i = 0; i < n; i++) printf(" %d", scan[i]);
      printf("\\n");
    }
  return 0;
}
`;
fs.writeFileSync(path.join(work, "probe.c"), probe);
sh([...cc, "-O0", path.join(work, "probe.c"), path.join(AVM_BUILD, "libavm.a"), "-o", path.join(work, "probe"), "-lm", "-lpthread"]);
const out = sh([path.join(work, "probe")]).split("\n").filter(Boolean);
fs.rmSync(work, { recursive: true, force: true });

// ---- emit -----------------------------------------------------------------
const enums: { name: string; value: number; file: string }[] = [];
const missing: string[] = [];
const tabs: { rust: string; dims: number[]; vals: number[]; c: string }[] = [];
// Scans are deduplicated: SCAN holds each distinct array once, SCAN_START
// says where [tx_size][tx_type]'s begins.
const scanData: number[] = [];
const scanAt = new Map<string, number>();
const scanStart: number[][] = [];
for (const line of out) {
  const f = line.split(" ");
  if (f[0] === "missing") {
    missing.push(f[1]);
    continue;
  }
  if (f[0] === "scan") {
    const [sz, ty, n] = f.slice(1, 4).map(Number);
    const vals = f.slice(4).map(Number);
    if (vals.length !== n || n === 0) die(`scan ${sz} ${ty}: ${vals.length} of ${n} values`);
    const key = vals.join(",");
    if (!scanAt.has(key)) {
      scanAt.set(key, scanData.length);
      scanData.push(...vals);
    }
    (scanStart[sz] ??= [])[ty] = scanAt.get(key)!;
    continue;
  }
  if (f[0] === "define") {
    enums.push({ name: f[1], value: Number(f[2]), file: "#define" });
    continue;
  }
  if (f[0] === "enum") {
    const n = names.find((x) => x.name === f[1])!;
    enums.push({ name: f[1], value: Number(f[2]), file: n.file });
  } else if (f[0] === "table") {
    const rank = Number(f[2]);
    const dims = f.slice(3, 3 + rank).map(Number);
    const vals = f.slice(3 + rank).map(Number);
    if (vals.length !== dims.reduce((a, b) => a * b, 1)) die(`${f[1]}: ${vals.length} values for dims ${dims}`);
    const t = tables.find((x) => x.rust === f[1])!;
    tabs.push({ rust: f[1], dims, vals, c: t.field ? `${t.c}[*].${t.field}` : t.c });
  }
}
tabs.push({ rust: "SCAN", dims: [scanData.length], vals: scanData, c: "av2_scan_orders[*][*].scan, each distinct array once" });
tabs.push({ rust: "SCAN_START", dims: [scanStart.length, scanStart[0].length], vals: scanStart.flat(), c: "where each av2_scan_orders[tx_size][tx_type] starts in SCAN" });
if (missing.length) die(`no #define in AVM's headers for: ${missing.join(", ")}`);
const rtype = (vals: number[]) => {
  const lo = Math.min(...vals), hi = Math.max(...vals);
  if (lo >= 0) return hi <= 255 ? "u8" : hi <= 65535 ? "u16" : "u32";
  return lo >= -128 && hi <= 127 ? "i8" : lo >= -32768 && hi <= 32767 ? "i16" : "i32";
};
const nest = (dims: number[], vals: number[], ind: string): string => {
  if (dims.length === 1) return `[${vals.join(", ")}]`;
  const stride = vals.length / dims[0];
  const inner = Array.from({ length: dims[0] }, (_, i) => ind + "    " + nest(dims.slice(1), vals.slice(i * stride, (i + 1) * stride), ind + "    "));
  return `[\n${inner.join(",\n")},\n${ind}]`;
};
let rs = `// @generated by rust/gen/defs.ts from AVM v1.0.0 (${AVM_COMMIT}). Do not edit;
// rerun the generator. Enum values are what AVM's compiler evaluates them to;
// tables are AVM's static tables, printed by a probe compiled against AVM.

#![allow(dead_code, non_upper_case_globals)]

`;
let lastFile = "";
for (const e of enums) {
  if (e.file !== lastFile) {
    rs += `\n// ${e.file}\n`;
    lastFile = e.file;
  }
  rs += e.value >= 0 ? `pub const ${e.name}: usize = ${e.value};\n` : `pub const ${e.name}: i32 = ${e.value};\n`;
}
rs += "\n// Tables\n";
for (const t of tabs) {
  const ty = t.dims.reduceRight((acc, d) => `[${acc}; ${d}]`, rtype(t.vals));
  rs += `\n/// ${t.c}\npub static ${t.rust}: ${ty} = ${nest(t.dims, t.vals, "")};\n`;
}
fs.writeFileSync(OUT, rs);
console.log(`wrote ${path.relative(process.cwd(), OUT)}: ${enums.length} enum constants, ${tabs.length} tables`);
