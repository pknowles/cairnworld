# Core Standards

Fail fast and loudly, with accurate error messages. Ideally no debugging is ever
needed - when something goes wrong we should simply know exactly what went wrong
and why. Always propagate errors. Only ever add to the error context. Never
ignore or attempt to translate or simplify errors. It is a mistake to lose
information in an attempt to hide technical details from users.

Ownership is in the object itself. Processing happens in the constructor. The
data lives on in the object. Name the object after the data it holds. Do not
create "manager" or "<verb>-er" objects - this is an anti-pattern.

Prefer complete-at-initialization, RAII, as rust is designed for. This
guarantees dependencies exist first, before dependent objects are constructed.
Any tension likely indicates a design issue that the compiler and language is
helping expose. This implies:
- No optional values
- No delayed initialization
- Prefer composition over inheritance

Less is more. The right solution is straightforward and simple, implicitly
handles all use cases and edge cases. Keep file sizes small and the number of
files small. Every line of code added (i.e. our code) is a chance for a bug and
incurs maintenance cost and tech debt. Make it count.

Prefer canonical off-the-shelf libraries that have been proven and are well
tested; avoid hand rolling things even if they sound simple. Pick the option
that literally has less lines of our own code.

Object ownership should be consistent with object lifetimes.

Reuse API objects rather than wrapping, translating and copying memory.

Performance is important. Pipeline processing, avoid unnecessary stalls and
synchronization, particularly GPU stalls. Simply reference, don't copy memory
unless there is no other way.

If there is a bug, issue, incomplete feature, fix the underlying issue at is
source. This may require refactoring or re-architecting in a way that handles
all use-cases holistically. Fallbacks, retry-loops, "defensive coding", timeouts
are frequently code smells that point to a bandaid instead of a real fix.

Prefer waiting and callbacks over polling. A timeout is a common code smell. If
an operation fails, we must know immediately.

Fail fast. Report and continue for non-critical feature must visibly display an
error with context and should immediately fail testing. Fallbacks, warnings, and
retry loops are all code smells that delay seeing problems and make debugging
many times harder. If a critical service fails to start, do not continue and
pretend everything is okay. Fail fast and loudly.

# Testing

## Philosophy

- Test the intent of the expected outcomes, not the code's implementation. I.e.
  if we improve the implementation and the result is still valid, testing should
  still pass. Exact values are only valid when they come from the spec, not from
  the implementation.
- Don't blindly write tests to check them off the list. Tests must verify real
  logic to ensure project goals are met.
- Do not test that the code does what the code does. This is far worse than
  useless. See the writing tests section below.
- Don't copy or reimplement project code in tests. Tests must test real project
  code. Consolidate utility functions with production where appropriate.
- Fix broken tests/lint warnings immediately, even if seemingly unrelated to
  your current step. Stop and debug. Claiming its "pre-existing" doesn't help.
  Do not commit with failing tests. Do NOT skip tests or relax test thresholds
  without explicit sign-off from the user.
- The project comes first. Don't sacrifice good code to make testing easier,
  although this may be a sign to improve modularity.

## Mocking

To mock or to write an end to end test? This should not be something you need to
ask. Simple answer. Either:

1. You can achieve the same level of coverage with appropriately simple mocking.
   Do that, it'll be faster to run and tangle less.
2. You'd have to mock an entire API or shortcut real project code, so you
   wouldn't be getting coverage anyway and should be pulling in more real code
   to test properly.

If your mocked object has to do non-trivial stuff (e.g. duplicate real code) you're not mocking, you're reimplementing something in test code.

If mocking is hard, causing problems, affecting how you design the main project code, STOP! You're doing it wrong and need something a level up and more end to end.

## Writing tests for LLMs

Some ideas and a structured process to help brainstorm for testing and weed out
unnecessary busy work tests.

1. List all the intended use cases, the happy paths.
2. List all the edge cases for each of the happy paths.
3. Describe the expected outcomes for each case and edge case
4. Describe two or three ways you can test whether the expected outcome happened or not, explaining why they verify the outcome and detect anything other than the outcome - your validation ideas
5. Anything else you can think of?

Stop here and ask the user to check your progress.

6. For each of the use cases, describe one result that would be incorrect. Would your test catch the wrong output?
7. For each of your validation ideas, evaluate how well they will work. Do they actually verify whether the feature works in spirit? I.e. not just that the code does what it does. Do they test solid invariants that match the use cases, allowing for the implementation to change yet still verify it produces the right result?
8. Replace any useless tests you found with test that would catch incorrect usage and repeat the above negative testing thought experiment.
9. Review to ensure we’re not adding brittle, tautological tests that would fail on valid implementation changes

Review before and after implementing.

10. Do your tests test real project code? I.e. you're not reimplementing anything.
11. Do your tests verify meaningful output and results? I.e. they are not verifying implementation details and are not just touching code for "coverage".
12. Do you have a broad sampling of input data to pipeclean all the code paths, both common and edge cases?
