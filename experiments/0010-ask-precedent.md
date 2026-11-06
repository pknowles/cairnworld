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
