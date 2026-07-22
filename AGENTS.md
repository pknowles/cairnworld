# Project Overview

This is a multiplayer text-based web-hosted RPG game that is run by LLMs and
implemented in rust.

# Worktree and git sanitation

Make a note of the git repository state before beginning any work. Do not let
uncommitted work become tangled. Commits must self-contained and individually
contribute features and fixes. If incomplete work exists, you must stop and deal
with it first. Faster now than later.

Some files may be temporary or intentionally un-tracked. You may use
.git/info/exclude for these, not .gitignore. Never use `git add -A`; prefer `git
add -u`, be surgical and explicitly add new files.

# Pre-commit checklist

The following items must have been completed before making a commit. This is a
hard gate and MUST be followed:

1. Documentation must be updated to match the changes
2. Appropriate testing has been run and passes
3. Prompt/context/model-facing changes have been exercised through the real chat
   path, including presenting edge case chats and evaluating related use-cases
4. Self-review performed:
   - Did you do everything agreed upon?
   - Anything missed or shortcuts taken?
   - Did you follow the rules here and in coding_standards.md and prompt_standards.md?
   - If you changed any LLM prompt, context packet, tool description, or model-facing instruction, did you complete the review checklist in prompt_standards.md?
   - Are your changes project-consistent, modular and did not introduce duplication?
   - Did you "fix" anything without evidence, i.e. proving the thing you fixed was actually the cause and true underlying problem?
   - Did you write any workarounds or bandaids that only fix a specific symptom, i.e. without finding the true cause needed for a robust solution? E.g. evidence-less, speculative or defensive "just in case" code without user sign off?
   - Anything else the user should know about?

# Index

## coding_standards.md and prompt_standards.md

Read these before doing anything. Re-read again before reviewing completed work.
These files contain strict rules that must be followed to keep the
implementation clean, help agents stay on track and avoid common pitfalls.

Common themes are:
- Implement and fix things holistically. One simple system rather than a pile of
  conditions, case and edge case handling and bandaids. Every line of code has a
  cost. Find a single and concise way.
- Everything must be verified through measurement and experimentation. Are there
  two ways to do something? E.g. before adding a feature you have multiple ways
  to implement and when changing or fixing something you also have the existing
  thing. Find a way to prove one version is better than the others for all the
  use-cases it supports, not just the specific case you might be thinking of or
  fixing.
- Test effectively and efficiency. End-to-end testing with the real thing is
  king. A single unit test to exercise just the code you're working on is good,
  but save big complete testing runs for infrequent checkpoints, e.g. commits.

## user_declarations.md

This file contains my top level level ground truth of what the project should
be, how it should work and any implementation requirements.

It is forbidden for any agent to edit it, even if it's implied it's ok.

Agents can absolutely draw the user's attention to it, suggest additions,
updates, corrections or point out contradictions or issues. Notify the user
about contradictions immediately.

## user_exerpts.md

When the user says something important, their direct quote may be added to
user_exerpts.md by agents to help guide the design and implementation. The idea
is for this file to contain clarifications, design ideas and edge cases that
flow from conversation snippets that are too detailed or fine grained to be
useful in user_declarations.md. It is natural for design to change over time, so
if any changes are made that result in contradictions, add a note saying the
idea was updated, with a date.

## design.md

This file contains the high level concepts and ideas of the project and the
overall approach to implementation. It is a structured consolidation of
user_declarations.md and a place for implementing agents to expand ideas and
fill in the gaps.

## implementation_details.md

The specific implementation references go here. Consider this an index to both
reference and detail how components in design.md are realised.
