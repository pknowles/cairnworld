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
add -u`, be surgical and explicitly add new files. Use temporary commits rather
than git stash or copying files as they are far more robust and there is less
risk of losing anything.

Use integrated edit/search tools rather than grep/sed when at all possible.

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
   - Did you follow the rules here and in coding_standards.md and
     prompt_standards.md?
   - If designing/planning, verify the implementation matches a direct user
     requirement and that there was no better and more straight forward way (see
     design.md below). Check that the design aligns with the coding standards.
     Does the implementation ordering flow so data and dependencies will simply
     already exist when needed or are constructs being introduced unnecessarily
     that would simply not be needed if the ordering were corrected or steps
     were consolidated?
   - If you changed any LLM prompt, context packet, tool description, or model-facing instruction, did you complete the review checklist in prompt_standards.md?
   - Are your changes project-consistent, modular and did not introduce duplication?
   - Did you "fix" anything without evidence, i.e. proving the thing you fixed was actually the cause and true underlying problem? See Debugging below for details.
   - Did you write any workarounds or bandaids that only fix a specific symptom, i.e. without finding the true cause needed for a robust solution? E.g. evidence-less, speculative or defensive "just in case" code without user sign off?
   - Did you skip or relax tests that resulted in important coverage lost?
   - Anything else the user should know about?

All issues should be fixed appropriately before committing.

# Debugging

LLMs frequently make wild guesses/speculations. That's fine, but DO NOT act on
them. For each,

- Describe a testable experiment that would either prove your hypothesis or
  further bisect the problem
- Would the outcome of the experiment actually tell you useful information? I.e.
  after knowing would you have proof or have at least ruled in/out some
  possibilities.
- Actually run the experiment(s). Don't assume you know because you've looked at
  the code.
- If it's faster to simply try a fix, make sure it's clearly labelled as as
  temporary experiment that is not checked in, adding "TEMPORARY. DO NOT
  SUBMIT". Clean it up afterwards. It is imperative that experimental code not
  mingle with development code. A good solution is to make a temporary commit
  that you will amend later.
- Verify fixes actually fix the original issue by attempting to reproduce the
  original issue yourself. Given changes were made, verify impacted features
  (related to the changed code) still work.

# Implementation loop

The following describes one iteration of a loop to follow when performing a long
term debugging or feature implementation task. Before starting multiple loops
it's important to define the finish line. Exactly what needs to be verified
working before stopping. Do not stop until the work is complete, tested,
reviewed and committed, or there is a real blocker.

- Define the goal for this iteration. Take stock and assess the current state.
  This is the time to see the forest for the trees. Have you been working
  effectively? Are the current plans working smoothly or are you having to jump
  through hoops to follow it when it's actually flawed design? Do you need to
  make changes to your strategies to avoid going down unnecessary rabbit holes
  and getting stuck? Course correct if needed, evaluate possible refactors,
  recognise when coding patterns are actually getting in the way. I.e. given all
  the requirements and use cases, what code structure would best fit, in a way
  that's modular, separable, composable, will allow for future changes and have
  low maintenance overhead.
- If debugging a problem, list your hypotheses and then list experiments you
  will perform to prove which is correct. I.e. don't fix speculations/guesses.
  See Debugging above.
- Plan what will be implemented or changed. Obviously don't write code otherwise
  that'd be an implementation and not a plan, but bullet point the changes that
  will need to be made. Include the motivation and intent. Check it aligns with
  the project documentation. Check the order of implementation works and that
  the plan doesn't require features that aren't there yet, or wouldn't be made
  simpler if a feature was there. Check that the granularity of the plan is
  appropriate - i.e. we won't need to write extra code just to have an
  intermediate step work and that we won't be implementing too much without
  modular testing in one big blob.
- Implement one complete slice that can be committed. See Worktree and git
  sanitation above. The project must be in a good state at the end so we can git
  bisect. You may need to revisit the plan or implement a little more to achieve
  this. I.e. do not over-correct and waste time forcing an intermediate state
  between complete features to work when we can just be coarser with our commits
  or have a bigger restructure. Reuse, refactor and update existing
  infrastructure and abstractions; don't add multiple similar implementations.
  Look for and remove dead code - it'll still be in the git history and again,
  every line of code has a cost! Take the time to do it right, don't take lazy
  shortcuts.
- Test the implementation. End to end is a must, even if you do this manually
  yourself. Do not check in code that has not been executed. If some code
  features are complex and risky, add unit tests for all the use cases with
  permutations of data. Balance testing with implementation speed. Testing must
  be fast and efficient, not testing useless invariants just for the sake of
  checking off testing. The goal here is to make the project succeed and perform
  the intent of the rules here, not satisfy the checklist, although the
  checklists can be helpful reminders.
- Self-review. See Pre-commit checklist above.
- Fix the issues found during the self-review appropriately, making sure to
  follow the coding standards while doing so.
- Commit. The commit body should simply be a short and concise list of changes
  made. The title should be a short summary.

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

Coding agents can absolutely draw the user's attention to it, suggest additions,
updates, corrections or point out contradictions or issues. Notify the user
about contradictions immediately. If the agent has reason to divert, this MUST
be brought to the user's attention to fix otherwise future agents will clobber
the change in decision since user_declarations is the highest level ground
truth.

Coding agents may need to make decisions that aren't implied by
user_declarations.md. In this case agents should suggest updates and
clarification be made to that document so that project design declarations
always flow from the top down.

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
fill in the gaps. TODOs are fine, but this is not a place to record progress or
current state.

Before writing plans from this document, check it for false assumptions. For
example it has sometimes described the addition of an entirely unnecessary
system/feature/approach, just because it was the first idea a coding agent wrote
down. Every decision must be traceable to a user declaration. If adding a new
system/framework/abstraction, make sure you have listed ~3 alternatives,
evaluated each against the coding standards and picked the most appropriate.

## implementation_reference.md

The specific implementation references go here. Consider this an index to both
reference and detail how components in design.md are realised. It must be kept
up to date before committing changes.

## plans/

Implementation planning records. Build order and status live here. Each plan is
one self-contained increment with a goal, scope, steps, per-step verification
and possibly a status line kept current. Completed plans may remain as records,
however it is expected they become stale. Do NOT refer to them as reference or
attempt to maintain them as references. In fact deleting them once they are in
git, completed and stale is probably best to avoid confusion given they'll be in
the git history.  Avoid mentioning history in both code and documents - that's
what git is for. Feel free to refactor plans inline if we pivot - it is only
going to lead to confusion if they are considered set in stone. The document
flow is: user_declarations.md (ground truth) → design.md (desired end state) →
plans/ (order, detail and status) → implementation_reference.md (index of what
exists). Naming them with a date prefix may help to know their order and what's
most recent.
