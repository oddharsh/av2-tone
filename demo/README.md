# demo

Encode one of your photos as AV1 and as AV2 at the same file size, look at
both, and race the encoders and decoders on it.

```bash
demo/build.sh                       # dav2d at a pinned commit, plus the dav1d and dav2d benches
bun demo/run.ts ~/photo.jpg         # about 6 minutes for a 1600px photo at speed 6
bun demo/serve.ts                   # http://localhost:8813
bun demo/race.ts                    # live decoder race over every photo run.ts encoded, about 20 s
```

`run.ts` also needs `encoder/build.sh --tuned` and the fast decoder's build.
Point it elsewhere with `ENCODER_BUILD=<dir>` and `FAST_DECODER_BUILD=<dir>`.
Install `ssimulacra2` (`brew install ssimulacra2`) and the page shows a
quality score for each file.

## What a run does

1. Your photo becomes an upright PNG, 1600px on its long side (`--size`).
2. AV1 at the settings aadhar.sh ships: aom, `-q 63`, 10-bit 4:4:4, speed 2.
   Its size is the budget.
3. AV2 with the tuned encoder, its quantizer searched until the file lands
   within 3% under that budget. Two or three encodes, usually.
4. **Encoder race.** Pristine AVM and the tuned build encode the same
   settings side by side, so both see the same load. The files must match
   byte for byte, since the tuned build's patch only changes speed.
5. **Decoder race,** best of 9, at 1 thread and at 8, decoding the raw OBUs
   in process: AVM, the fast decoder, and dav2d. dav2d refuses 4:4:4, so all
   three also race on a 4:2:0 encode of the same photo. dav1d decodes the AV1
   file for scale. Every AV2 decoder has to produce the same pixel hash, or
   the run fails.
6. Both files decode to PNG for the page, since no browser reads AV2.

## Reading the numbers

Each run records the load average at its start and end. Run it on a quiet
machine if you want numbers to quote: the AV2 encoder uses about one core
per photo, and the decoders' thread pools suffer most when other work holds
the cores. The encoder race is one encode per arm; the decoder race is best of
`--reps`.

## The live race

`race.ts` skips encoding: it races the decoders on whatever `run.ts` already
encoded, so it starts at once. The screen redraws after each decode, checks
that every AV2 decoder wrote the same pixels on every photo, then replays the
race at each decoder's measured speed in real time. It pins every decoder to
one core by default; `--threads 8` races thread pools instead, and
`--slowmo 4` stretches the replay. Name stems to race a subset.
