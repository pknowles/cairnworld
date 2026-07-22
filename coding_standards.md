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
files small. Every line of code added is a chance for a bug and incurs
maintenance cost and tech debt. Make it count.

Object ownership should be consistent with object lifetimes.

Reuse API objects rather than wrapping, translating and copying memory.

Performance is important. Pipeline processing, avoid unnecessary stalls and
synchronization, particularly GPU stalls. Simply reference, don't copy memory
unless there is no other way.

Always propagate errors. Only ever add to the error context. Never ignore or
attempt to translate or simplify errors. It is a mistake to lose information in
an attempt to hide technical details from users.

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
