# Overall Guidance

This project must work with small LLMs, so keeping context short and concise is
critical. Prompts, tool descriptions and context packets all consume model
attention, which is very valuable and limited. Every addition has a cost; make
sure it earns its place.

In most cases, a failed LLM result can be solved with relatively subtle changes.
The first thing to look for is subtle or implied contradictions, rules, actions,
conclusions etc. Investigation must begin with a review of the LLM's context at
the time it produced inappropriate output.

If you do add anything it had better maintain strong importance consistency next
to existing context. I.e. no pointless or distracting fluff. LLM models operate
best with generic instruction. Avoid anything that smells of specific conditions
or natural language processing.

Give the model the smallest accurate world model it needs to reason and produce
an accurate result with high confidence and no more.

Provide facts that let the model infer a useful response over instructions that
prescribe a particular response. This allows the model to react appropriately in
unplanned and unexpected situations.

When model behavior is poor, first look for missing, stale, or contradictory
context. Add a rule only for a genuine policy or interface contract that cannot
be inferred from accurate context. Never add a transcript-specific rule to
compensate for facts the model was not given.

Do not attempt to validate and fix model output by hand. That would be like
trying to make AI with if-statements. Review and refine the input text, context
and chat history instead.

LLM tool calls MUST consume arguments that make sense. Don't try to plumb
through hidden input context and arguments behind the LLM's back. For example an
ID may be returned from one call and given to another. If the tool call implies
the last ID then you have hidden state and this will confuse the agent as it
won't understand how the tool knows which ID to use. If additional tool context
is truly implied, make sure it is documented for the tool caller.

# Prompt Guidelines

Keep prompts concise and purposeful:

- Every sentence should have a concrete and unique job.
- Put the most important action guidance first.
- Prefer positive action framing: describe the desired behavior directly.
- Rarely use negative "do not" statements and treat any you find as suspicious
  and evaluate for removal. Instead, search for the root cause - e.g. stale
  wording, contradictory context, missing state, or a confusing tool surface. If
  the root cause is unknown, establish it with evidence before changing the
  prompt.
- Use examples only when they teach a reusable pattern or disambiguate an
  otherwise ambiguous interface.
- Avoid listing or even discussing edge cases unless the model truly needs them
  for the common path. LLMs often over-focus and pattern match, which may make
  them do exactly what the text tells them not to.
- Avoid describing private implementation details unless the model must use
  them to choose the right visible action.

After adding prompt text, review for possible consolidation and redundancy.
Prefer removing stale, contradictory, or redundant context over adding new
instructions. Less is more.

## Prompt review checklist

- What exact failure or capability gap justified this prompt change?
- Did you verify the prompt changes are a net gain, producing the desired result
  and not harming any other required behaviour?
- Does the model have the context it needs to act appropriately in all
  use-cases?
- Is there any opportunity to consolidate or shorten the prompt without losing
  meaning?
- Are there any instructions where more accurate context would instead imply the
  desired action instead?
- Can the same outcome be achieved by removing or correcting existing context?
- Is each added sentence necessary for the normal path?
- Does each sentence express a broad, useful rule, or is it narrowly reacting to
  one recent event?
- Does the wording guide the model toward the next useful action without drawing
  attention to rare failure modes?
- Are tool names, available capabilities, and examples consistent with the
  actual tool boundary visible to that agent?
- Did a workflow example accidentally become a mandatory sequence, hiding other
  actions that remain valid?
- Did you test to verify the new prompt assembly?
- Did a real chat through the normal product path verify the intended model
  behavior with the intended provider, visible tools, and backend?
- Is an LLM doing something that is fully deterministic that would be better
  done by the programming language? Inference is expensive and best for fuzzy
  interpretation and tasks.
