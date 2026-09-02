# Cost of storing the verbatim rendered prompt

The rendered prompt - the fully templated text the model actually receives -
was the only thing that revealed the dropped-tool-call bug (experiment 0001).
Reconstruction from the recipe gives back the `Request` we assembled, not the
string the template produced from it, so a bug between those two is invisible
without keeping the rendered form. This measures what keeping it costs.

## Measurements

Captured from real runs with one `save` tool and a GM system prompt, one user
turn. Each inference in a turn re-sends the whole conversation, so size grows
with the square of the turn count, not linearly.

| Run | Prompts | Raw bytes |
|---|---|---|
| Qwen3, resolved in 2 inferences | 2 | 3,753 |
| Llama 3.1, 8 inferences (pre-fix, repeating) | 8 | 28,516 |

Compression of the 8-prompt Llama capture:

| Scheme | Bytes | % of raw |
|---|---|---|
| raw | 28,164 | 100% |
| prefix deltas against the previous prompt | 4,240 | 15.1% |
| gzip, whole file | 1,303 | 4.6% |
| gzip, each prompt independently | 8,332 | 29.6% |

Qwen3 capture: raw 3,753, whole-file gzip 981, per-prompt gzip 1,797.

## What the numbers say

Most of the redundancy is *between* prompts, not inside one. Each prompt is
close to a prefix extension of the previous one, so compressing rows
independently - the natural fit for one row per inference - recovers only 29%,
while compressing them together recovers 4.6%.

Prefix-delta encoding by hand lands at 15%, worse than simply gzipping the
group, and costs code we would own. Grouping rows into a compressed block and
indexing into it beats both, at the cost of a block format and an index.

## Open decision

Not yet needed: nothing stores rendered prompts today. When milestone 8's
inference view needs them, the options in cost order are:

1. Keep a short window of recent prompts and delete older ones. Reconstruction
   from the recipe still works for anything older; only the exact rendered
   string is lost.
2. Compress groups of prompts into blocks with an index, per the archiving
   requirement already in user_declarations.md ("extract and archive them by
   date or age", "archiving with compression should be efficient").
3. Per-row compression. Simplest to fit the schema, worst ratio of the three.

Option 1 costs almost no code and loses only old rendered text; option 2 is the
same machinery archiving already needs.
