# The GM repeats its previous decision on an ask

The real GM (Qwen3.5-4B, production sampling, a seed per repeat, fixed player names) is put six
submissions as asks in one conversation: two real questions, and four that are not (speech by Toma,
a request from Mara, Toma's action, a question about the end of the story). Right is answering the
real questions and rejecting the rest. `CAIRNWORLD_REPEATS=N` varies the seed.

Reading a failing input: the third submission, Toma's line of speech, came after two earlier
submissions in the GM's history that it had answered ("Answered." in each result). It answered the
third the same way.

| Wording | Order | Wrong |
|---|---|---|
| Original ("asks you", "sent you this as a question") | questions first | 3 of 6 (one run, random names) |
| Neutral ("submitted"), the rule in the role | questions first | 2 of 24 (4 seeds) |
| Same | non-questions first | 8 of 8 real questions rejected; the 16 non-questions all rejected |
| The rule repeated in each submission | questions first | the turn ended without narrating |
| Same | non-questions first | 8 wrong |

The GM's decision follows its previous decision, not the new submission. Wording did not change that.

## Each ask in a conversation of its own

The fix this points to is judging each ask without earlier asks in its history. To check before
building it, the same six asks were run each in a fresh game (4 seeds, 24 decisions):

| Ask | Wrong |
|---|---|
| Both real questions | 0 of 8 |
| Toma's speech, Toma's action, the end of the story | 0 of 12 |
| "Mara asks the Adventurer to retrieve the flour from the hut." | 4 of 4: answered |

So the order-dependent errors go: with no earlier submissions to copy, the GM judged every ask but one
correctly. The remaining one is not a precedent effect. It was rejected correctly in the run of asks
only because the submission before it had been rejected, so precedent decided that case by accident.
A submission that reads like a question and is a request to someone else ("Mara asks ...") is
answered. A fresh context gives the model's own accuracy on a text (20 of 24 here), not 24 of 24.
