// av2trace.c: every symbol pristine AVM reads from a still's tiles, in order.
// Linked against an AVM built with CONFIG_ACCOUNTING=1 (a stock AVM build
// option, not a patch), which records each entropy-coded read with its value,
// how it was coded, its cost and the block it belongs to. Prints
//
//   sym <id> <mi_col> <mi_row> <tree> <mode> <value> <bits>
//          one line per symbol, in decode order. mode is AVM's
//          SYMBOL_CODING_MODE (0 bool, 1 bypass bit, 2 bypass literal,
//          3 unary, 4 CDF); bits is the cost in 1/65536 bits
//   name <id> <function> <line> [tags]
//          what AVM's source calls each symbol id
//
// so av2-check can find the first symbol where av2-tone's parse and AVM's
// part ways, and name it. With --digest it prints one line instead,
//
//   digest <symbols> <fnv1a-64>
//          over each symbol's mi_col, mi_row, value (int32), tree, mode
//          (uint8) and bits (uint64), little-endian, in that order
//
// which the Rust tests keep for a few stills so `cargo test` checks the parse
// against AVM without AVM.
//
//   av2trace [--digest] <file.obu>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "avm/avm_decoder.h"
#include "avm/avmdx.h"
#include "av2/decoder/accounting.h"

static uint64_t fnv(uint64_t h, uint64_t v, int bytes) {
  for (int i = 0; i < bytes; i++) {
    h ^= (v >> (8 * i)) & 0xff;
    h *= 0x100000001b3ull;
  }
  return h;
}

int main(int argc, char **argv) {
  const int digest = argc == 3 && !strcmp(argv[1], "--digest");
  if (argc != 2 && !digest) {
    fprintf(stderr, "usage: av2trace [--digest] <file.obu>\n");
    return 2;
  }
  FILE *in = fopen(argv[argc - 1], "rb");
  if (!in) return 2;
  fseek(in, 0, SEEK_END);
  const long n = ftell(in);
  rewind(in);
  uint8_t *buf = malloc(n > 0 ? (size_t)n : 1);
  if (!buf || fread(buf, 1, (size_t)n, in) != (size_t)n) return 2;
  fclose(in);

  avm_codec_ctx_t ctx;
  avm_codec_dec_cfg_t cfg = { 0 };
  cfg.threads = 1;
  if (avm_codec_dec_init(&ctx, avm_codec_av2_dx(), &cfg, 0)) return 1;
  if (avm_codec_decode(&ctx, buf, (size_t)n, NULL)) {
    const char *d = avm_codec_error_detail(&ctx);
    fprintf(stderr, "AVM: %s\n", d ? d : "decode failed");
    return 1;
  }
  Accounting *acct = NULL;
  if (avm_codec_control(&ctx, AV2_GET_ACCOUNTING, &acct) || !acct) {
    fprintf(stderr, "this AVM has no accounting; build it with CONFIG_ACCOUNTING=1\n");
    return 1;
  }
  const AccountingSymbols *s = &acct->syms;
  if (digest) {
    uint64_t h = 0xcbf29ce484222325ull;
    for (int i = 0; i < s->num_syms; i++) {
      const AccountingSymbol *y = &s->syms[i];
      h = fnv(h, (uint32_t)(int32_t)y->context.x, 4);
      h = fnv(h, (uint32_t)(int32_t)y->context.y, 4);
      h = fnv(h, (uint32_t)(int32_t)y->value, 4);
      h = fnv(h, (uint8_t)y->context.tree_type, 1);
      h = fnv(h, (uint8_t)y->coding_mode, 1);
      h = fnv(h, (uint64_t)y->bits, 8);
    }
    printf("digest %d %016llx\n", s->num_syms, (unsigned long long)h);
    avm_codec_destroy(&ctx);
    free(buf);
    return 0;
  }
  for (int i = 0; i < s->num_syms; i++) {
    const AccountingSymbol *y = &s->syms[i];
    printf("sym %u %d %d %d %d %d %llu\n", y->id, y->context.x, y->context.y, (int)y->context.tree_type,
           (int)y->coding_mode, y->value, (unsigned long long)y->bits);
  }
  for (int i = 0; i < s->dictionary.num_strs; i++) {
    const AccountingSymbolInfo *a = &s->dictionary.acct_infos[i];
    printf("name %d %s %d", i, a->c_func, a->c_line);
    for (int t = 0; t < AVM_ACCOUNTING_MAX_TAGS; t++)
      if (a->tags[t]) printf(" %s", a->tags[t]);
    printf("\n");
  }
  avm_codec_destroy(&ctx);
  free(buf);
  return 0;
}
