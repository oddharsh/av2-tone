// build.sh's check of patch 0012: a user-defined matrix set equal to AVM's
// own must quantize as AVM's own does. AVM's predefined forward weights are
// round(1024 / iwt); before 0012 the user-matrix path truncated, which moved
// 38% of entries. Loads every level's predefined 8x8, 8x4 and 4x8 through
// av2_qm_frame_update (what the encoder runs for a qm-file; the unused
// av2_qm_replace_level leaves gqmatrix alone) and compares the forward weights
// at every size those reach but 4x4, which AV2 derives from the 8x8 and so
// differs on its own. Prints the mismatches and exits 1 if there are any.
#include <stdio.h>
#include <string.h>

#include "av2/common/av2_common_int.h"
#include "av2/common/quant_common.h"

int main(void) {
  static CommonQuantParams own, user;
  av2_qm_init(&own, 3);
  av2_qm_init(&user, 3);
  const TX_SIZE fund[3] = { TX_8X8, TX_8X4, TX_4X8 };
  long bad = 0, n = 0;
  for (int level = 0; level < NUM_CUSTOM_QMS; ++level) {
    qm_val_t ***set = av2_alloc_qmset();
    for (int t = 0; t < 3; ++t)
      for (int c = 0; c < 3; ++c)
        memcpy(set[t][c], own.giqmatrix[level][c][fund[t]],
               tx_size_2d[fund[t]] * sizeof(qm_val_t));
    av2_qm_frame_update(&user, 3, level, set);
    av2_free_qmset(set);
    for (int c = 0; c < 3; ++c)
      for (int t = 0; t < TX_SIZES_ALL; ++t) {
        if (t == TX_4X4 || av2_get_adjusted_tx_size(t) != t) continue;
        if (tx_size_wide[t] > 8 || tx_size_high[t] > 8) continue;
        const qm_val_t *a = own.gqmatrix[level][c][t], *b = user.gqmatrix[level][c][t];
        for (int i = 0; i < tx_size_2d[t]; ++i, ++n) bad += a[i] != b[i];
      }
  }
  printf("qmround: %ld of %ld forward weights differ from AVM's own\n", bad, n);
  return bad != 0;
}
