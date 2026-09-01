# Pure destructors

**The design is `rfc/model/gc/pure-destructors.md`**, which is normative
and maintained. This file is the working note that stays behind, as that
document's own header says.

Proposed by Edmond on 2026-08-18: an object whose destructor is absent,
or provably affects only the object's own data, is pure, and a pure
object could be reclaimed by the collector itself. The analysis of the
same day — three lenses, then two Critic rounds — moved to the RFC on
2026-08-20 and was amended there on 2026-08-23, when Edmond restated
ruling 5: the mutator frees. That amendment withdraws the hand-off drain
this note originally recommended, so the analysis is read there and not
from a copy.

What is open, and who owns it: `PLAN.md`'s backlog line "Pure
destructors, and the hand-off drain". The runtime-only step needs no
ruling and no compiler; the hand-off waits on the residual-duties and
tail-bound questions the RFC's open list names.
