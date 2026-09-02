# Llama 3.1 8B Instruct tool-call behaviour

Facts and test results only. Hardware: RTX 3070 8GB. Model:
`Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf` (bartowski), mistralrs 0.8.1, CUDA.
Sampling: temperature 0.7 unless stated.

**Experiments 1-3 ran against a harness that silently dropped every assistant
tool call from the prompt** (see "Resolved: the repetition was a harness bug").
Their observations of what the model emitted are still accurate, but no
conclusion about model quality drawn from them holds, because every model was
answering a conversation missing its own previous turn. Re-run anything that
matters before relying on it.

Method note: those experiments reasoned about the prompt from the template
source rather than reading what was sent. A reconstruction built that way
showed the tool call present, because it reproduced the intended behaviour
rather than the actual one. The bug only appeared once the rendered prompt was
captured from the running system.

## Source of truth: the model's own chat template

Read from the GGUF `tokenizer.chat_template` metadata key (4614 chars).

- When `tools` is non-empty the template injects `Environment: ipython` into
  the system message.
- The template instructs: `Respond in the format {"name": function name,
  "parameters": dictionary of argument name and its value}. Do not use
  variables.` This is bare JSON with no wrapper token.
- The tool list is injected into the **first user message**, not the system
  message. The template raises `Cannot put tools in the first user message
  when there's no first user message!` when no user message exists.
- `<|python_tag|>` is emitted by the template only when a called tool name is
  in `builtin_tools`. Custom tools use the bare-JSON form above.
- The template raises `This model only supports single tool-calls at once!`
  when an assistant message carries more than one tool call.

## Parser behaviour in mistralrs 0.8.1

From `mistralrs-core-0.8.1/src/tools/mod.rs`:

- `contains_tool_call_prefix` recognises `<tool_call>`, `<|tool_call>`,
  `<｜tool▁call▁begin｜>`, `<|python_tag|>`, `[TOOL_CALLS]`.
- The Llama branch is `message.strip_prefix("<|python_tag|>")`, which only
  matches when the tag is at position 0 of the message.

## Experiment 1: system-prompt wording vs emitted call format

*Ran with tool calls missing from the prompt; see the note at the top.*

Three system prompts, three trials each, identical user turn
("Rook edges along the rotten ledge above the ravine. Does he make it?"),
one tool offered (`save`).

| Variant | System prompt addition | Calls per trial | `<\|python_tag\|>` |
|---|---|---|---|
| baseline | none | 6, 3, 3 | 0/3 |
| final | "A save is rolled once and its result stands. Narrate what the result means." | 0, 0, 0 | 3/3 |
| narrate | "After a tool returns, reply to the player in prose describing what happens." | 8, 3, 8 | 0/3 |

Outcomes:

- baseline: 1 run errored; 2 runs ended with bare tool-call JSON emitted as
  assistant text.
- final: all 3 runs failed at the first inference with "model returned both
  text and tool calls"; `<|python_tag|>` appeared mid-message in all 3.
- narrate: 2 runs reached the 8-inference chat limit; 1 ended with bare
  tool-call JSON as assistant text.

Result: the system prompt changed which tool-call format the model emitted
(0/3 vs 3/3 tag usage between baseline and final).

## The rendered prompt contains two conflicting format instructions

`tools_in_user_message` defaults to `true` in the template and mistralrs never
sets it, so tool definitions are injected into the first user message.
Rendering the template with jinja2 for one system message, one user turn and
the `save` tool produces a prompt containing both of these:

- In the system message: `Environment: ipython`
- In the user message: `Respond in the format {"name": function name,
  "parameters": dictionary of argument name and its value}.Do not use
  variables.`

`Environment: ipython` is the Llama 3.1 marker associated with `<|python_tag|>`
output. The user-message instruction specifies bare JSON with no wrapper token.
Both reach the model in the same request.

Note also the rendered text has no space between `variables.` and the preceding
sentence (`...its value}.Do not use variables.`), and the tool JSON is rendered
with the `{"type": "function", "function": {...}}` envelope rather than the
function object alone.

## Experiment 2: removing the conflicting instructions from the template

*Ran with tool calls missing from the prompt; see the note at the top.*

Method: `GgufModelBuilder::with_chat_template` (takes a path to a `.json` or
`.jinja` file) loaded a modified copy of the model's own template with
`tools_in_user_message` forced to `false` (tools rendered into the system
message) and the `Environment: ipython` line removed. Confirmed by rendering:
the resulting prompt carries the bare-JSON instruction only, with no `ipython`
marker. Same user turn, same system prompt, same tool, 3 trials.

| Trial | Tool calls | Inferences | `<\|python_tag\|>` | Error |
|---|---|---|---|---|
| 1 | 8 | 8 | 0 | hit 8-inference chat limit |
| 2 | 7 | 8 | 1 | model returned both text and tool calls |
| 3 | 4 | 4 | 0 | none |

Result: removing the conflicting format instructions did not stop repeated
calls. All three trials still issued 4 or more `save` calls for one hazard.

Trial 3 detail: the **first** save returned a pass ("Save resolved: Rook
succeeds..."), and the model issued three further `save` calls anyway. Repeated
calling is therefore not conditional on the save having failed.

## Candidate chat templates compared

Read from each GGUF's `tokenizer.chat_template` metadata key.

| Model | Template size | Tool-call support in template |
|---|---|---|
| Llama 3.1 8B Instruct | 4614 chars | yes, bare JSON + `Environment: ipython` |
| Qwen3 8B | 4614 chars (different file) | yes, 11 `tool_call` references, no `ipython` marker, `<think>`/`enable_thinking` present |
| Hermes 3 Llama 3.1 8B (NousResearch GGUF) | 291 chars | **none** |

The Hermes GGUF ships a bare ChatML template with no `tools` handling.
Rendering it with a tool list produces a prompt containing no tool definitions:
the tools are silently dropped and the model is never told the tool exists.
Testing tool-calling with this file measures the packaging, not the model.

## Open defect: tool results are quoted before the model sees them

Not the cause of the repetition below, but a real flaw, unfixed. The chat
template is stored inside the GGUF under `tokenizer.chat_template` (4614 chars
for Llama 3.1), so it ships with the weights rather than coming from the
library. Its tool-result branch is:

```jinja
{%- if message.content is mapping or message.content is iterable %}
    {{- message.content | tojson }}
{%- else %}
    {{- message.content }}
{%- endif %}
```

A plain string satisfies `is iterable`, so the `tojson` branch always wins and
every text result reaches the model wrapped in quotes: `"Save resolved: ..."`.
The `else` branch is unreachable for strings.

Correcting the test to `is mapping` alone fixes it. Verified by rendering the
same messages through both templates:

- published: `"Save resolved: Rook succeeds. This result is final."`
- corrected: `Save resolved: Rook succeeds. This result is final.`

A corrected template can be supplied with `--chat-template` or a
`chat_template` key on the model's config entry, so no code change is needed to
adopt it.

Left unapplied for now: the correction does what it claims, but it does not
change how the model behaves on the case that prompted the investigation
(experiment 3 below - 8 repeated calls both before and after). Adopting it
means carrying a hand-edited copy of the published template, so it is worth
doing when there is a behaviour it demonstrably improves - most likely once
tool results carry structured data rather than one sentence.

## Resolved: the repetition was a harness bug, not the model

Experiments 1-3 below tested the prompt and the tool result and found no cause.
They were all looking in the wrong place, because they reasoned about the
prompt rather than reading the one the model received.

Capturing the verbatim rendered prompt (a temporary dump at the return of
`apply_chat_template_to`) showed the assistant's tool call was missing
entirely:

```
<|im_start|>assistant
<|im_end|>
<|im_start|>user
<tool_response>
Save resolved: Rook succeeds to balance on the ledge. This result is final.
</tool_response><|im_end|>
```

An empty assistant turn, then a result with nothing explaining what was asked.
Reissuing the call is a reasonable response to that input.

Cause: `RequestBuilder::add_message_with_tool_call` stored the calls under a
`"function"` key while `chat_template.rs` and the templates read `"tool_calls"`
(`get_mut("tool_calls")`, `contains_key("tool_calls")`). Nothing read
`"function"` at message level, so the calls were silently dropped. Fixed in the
mistral.rs submodule.

Effect on the original case, same prompt and system prompt, 3 trials each:

| Model | save calls before | save calls after |
|---|---|---|
| Llama 3.1 8B Instruct | 8, 8, 8 (hit the chat limit) | 1, 1, 1 |
| Qwen3 8B | 1 | 1 |

The weather control (one tool offered, a question needing none) improved from 8
spurious calls to 1 for Llama 3.1.

Two model-quality observations survive the fix, both distinct from the bug:

- Llama 3.1's final reply after the fix describes the function and its
  parameters rather than narrating the outcome.
- Hermes 3 emits a `<tool_call>` block with no closing tag, so it is not parsed
  and leaks as text. Reproduced at temperature 0, so it is deterministic rather
  than sampling variance. It also read "Rook has DEX 11" as
  `attribute_value: 1`.

## Experiment 3: the repetition is not caused by the tool result

*Ran with tool calls missing from the prompt. Its conclusion was wrong: the
repetition had a cause, found later. See the resolved section above.*

Two candidate causes were tested and eliminated.

*Quoted tool results.* The corrected template above does remove the quotes the
model sees, so the input genuinely differs. The call count does not: 3 trials
at 8, 8 and 8 `save` calls, the same as with the published template.

*Result content.* In those runs the results alternated between "Rook succeeds"
and "Rook does not succeed" across repeated identical calls, and the model
continued calling regardless of which it received.

*Control.* With the same system prompt and the same single tool offered, but a
user turn that needs no tool at all ("Describe the weather in one sentence."):

| Model | `save` calls |
|---|---|
| Llama 3.1 8B Instruct | 8 (hit the chat limit) |
| Hermes 3 | 0 |
| Qwen3 8B | 0 |

Llama 3.1 called a save tool eight times when asked to describe the weather.
The other two models, given identical input through identical code, called
nothing. The repetition therefore does not depend on the save mechanic, the
result wording, or the prompt describing a hazard.

Note `strict: Some(true)` is set on every tool sent, which constrains generated
arguments to the schema, and Llama 3.1 still emitted `"attribute_value": "11"`
and `"difficulty": "0"` as strings.

## Observed malformations

All from the runs above, verbatim from recorded inferences:

1. Numbers quoted as strings: `"attribute_value": "11"`, `"difficulty": "0"`.
2. Markdown-escaped underscore in a key: `attribute\_value`.
3. Bare tool-call JSON emitted as assistant text with no wrapper token.
4. `<|python_tag|>` emitted after content had already started streaming.
5. Repeated identical `save` calls after a failing result (3, 6 and 8 calls
   for one hazard), with `reason` degrading across retries: "edges along the
   rotten ledge above the ravine" -> "edge along rotten ledge" -> "rook edge
   along the rotten ledge".
