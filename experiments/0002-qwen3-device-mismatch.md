# Qwen3 GGUF fails when device-mapped across CPU and GPU (mistralrs 0.8.1)

Hardware: RTX 3070 8GB. ~2.1GB is held by the desktop session (Xorg 1253MiB
plus browser/editor), leaving ~5.7GB free. mistralrs 0.8.1, CUDA feature.

## Symptom

Every inference fails at the prompt step:

```
model error during inference: device mismatch in rms-norm, lhs: Cpu, rhs: Cuda { gpu_id: 0 }
```

Reproduced with `Qwen_Qwen3-8B-Q4_K_M.gguf` (3 runs with tools, 2 without) and
with `Qwen_Qwen3-8B-Q3_K_M.gguf` (1 run). Independent of tools, prompt length
and temperature.

## Resolution

The fix lives in `third_party/mistral.rs`, a submodule of
https://github.com/pknowles/mistral.rs, and `Cargo.toml` depends on it by path.
That fork is at 0.9.0, which also moved `Function` to require a `strict` field
(`Some(true)` constrains generation to the argument schema via llguidance).

Verified from the committed tree, same prompt and system prompt for each, one
`save` tool offered, temperature 0.7:

| Model | save calls | Inferences | Errors |
|---|---|---|---|
| Llama 3.1 8B Instruct | 8 | 8 | 0 (hit the chat limit) |
| Hermes 3 (with `--chat-template templates/hermes-tools.jinja`) | 1 | 2 | 0 |
| Qwen3 8B | 1 | 2 | 0 |

## Cause, confirmed by patching

Adding the one missing line to a local copy of `mistralrs-core` 0.8.1 and
building against it via `[patch.crates-io]` makes Qwen3 run. Same binary
otherwise, same model file, same CPU/GPU split reported by the mapper.

Before the patch: every inference failed with the device mismatch.
After the patch, with no other change:

- `--temperature 0`, prompt "Say hello in three words." -> exit 0, reply
  "Hello! How can I assist you?"
- `--temperature 0.7`, GM system prompt, `save` tool offered, prompt "Rook
  edges along the rotten ledge above the ravine. Does he make it?" -> exit 0,
  one `save` call, one tool result, one final narration. 4 messages, 2
  inferences, 0 errors.

## Cause

`mistralrs-core-0.8.1/src/models/quantized_qwen3.rs` omits the device move that
`quantized_llama.rs` performs before the final norm.

`quantized_llama.rs`, after the layer loop:

```rust
            layer_in = x;
        }
        let layer_in = layer_in.to_device(&self.device)?;
        let x = self.norm.forward(&layer_in)?;
```

`quantized_qwen3.rs`, same position:

```rust
        }
        let x = self.norm.forward(&layer_in)?;
```

When the mapper places trailing layers on CPU, Qwen3 passes a CPU tensor to a
GPU-resident norm weight. Llama does not fail because it moves the tensor back
to `self.device` first. Both files otherwise share the same mapper handling
(`mapper.map(layer_in, i)` inside the loop).

This is an upstream defect, not a configuration error: no combination of
settings avoids it while any layer is on CPU.

## Device mapping observed

| Model | Repeating layers | GPU layers | CPU layers |
|---|---|---|---|
| Qwen3 8B Q4_K_M, `max_seq_len` 4096 (default) | 36 | 0-13 | 14-35 |
| Qwen3 8B Q4_K_M, `max_seq_len` 2048 | 36 | 0-13 | 14-35 |
| Qwen3 8B Q4_K_M, `max_seq_len` 512 | 36 | 0-14 | 15-35 |
| Qwen3 8B Q3_K_M (3.84 GiB), default | 36 | 0-18 | 19-35 |
| Llama 3.1 8B Instruct Q4_K_M, default | 32 | 0-15 | 16-31 |

The auto mapper reports "8 GB" for the GPU while only ~5.7GB is free, and
reserves 512MB (`GPU_MIN_RESERVE_BYTES`). A smaller quant moved the boundary
from layer 14 to 19 but did not avoid the split.

## Approaches tested and rejected

- `DeviceMapSetting::Auto` with `max_seq_len` 2048 and 512: still split.
- `Topology::with_range(0..n, LayerTopology { device: Some(cuda), .. })` for n
  in {36, 32, 28, 24, 20, 18, 16, 14, 8}: every value failed with
  `DriverError(CUDA_ERROR_OUT_OF_MEMORY)`, including n=8. The same call with
  Llama 3.1 at n=16 also OOMs, although the auto mapper places 16 Llama layers
  on GPU successfully. `Topology` does not express "these layers on GPU, the
  rest on CPU"; this path was a misuse of the API and measures nothing about
  Qwen3.
- Smaller quant (Q3_K_M, 3.84 GiB): still split, still fails.

## Ways forward, untested

- Free the ~2.1GB held by the desktop so all 36 layers fit on GPU.
- Patch `quantized_qwen3.rs` to add the missing `to_device` call (upstream fix
  or local patch).
- A model small enough to fit entirely on GPU alongside the desktop.

## Checked against upstream's reported issues

Searched a local dump of all 707 mistral.rs issues (234 open, 473 closed) for
both defects this project hit.

- The dropped tool call has never been reported. The nearest is #1687 (closed
  2025-11-04), which hits `add_message_with_tool_call` but reports a panic
  rather than calls being silently discarded from the prompt.
- The Qwen3 device mismatch has never been reported. #2160 (closed) is a
  different Qwen3 GGUF failure: MoE on Metal taking a CUDA-only path.
- #2278 (closed) concerns tools registered via `with_tool_callback_and_tool`
  not reaching the model. That is a different API from the one used here.

Neither defect had an existing report, so neither had an upstream fix waiting.
