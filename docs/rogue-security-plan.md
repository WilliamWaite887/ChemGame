# Rogue Security: chemistry under pressure

Status: **Deferred**. Implement conversation-first NPC orders, campaign greetings,
shared intake, and physical queues before returning to this design.

## Agreed direction

Corruption is occasional systemic workplace trouble alongside the main campaign,
not a new main antagonist or a finite investigation quest. On some saves Officer
Reyes is corrupt, using the existing hidden resident assignment. Warden Bex stays
accountable and provides a dependable appeal route. Honest chemists can encounter
pressure but always have workable options without supplying drugs or accepting
violence. Individual disputes resolve; the relationship continues.

## Recurring incidents

- **Unlogged requisition:** supply chemicals, offer a useful legitimate substitute,
  refuse, or insist on an accountable handover. Substitutes must meet the actual
  need; an explicit drug demand does not accept any convenient mixture.
- **Disputed seizure:** surrender a particular batch temporarily, negotiate its
  release, or challenge the claim using a sample and the incident record.
- **Protection bargain:** exchange supplies for a concrete favor, such as releasing
  a held batch or cancelling an inspection. The promised favor actually happens.

Start with one incident per 8–12 minutes of active work after the opening
deliveries, at most one unresolved corruption incident. Closing Chemistry pauses
new approaches; resolution always grants breathing room. Requests must match
chemicals the player can obtain.

## Choices and consequences

Cooperation spends supplies for relief or a favor and encourages future demands.
Negotiation uses real chemistry. Refusal may work or lead to a stated inspection
or hold, but does not itself lower legitimate Security standing. Evidence works
even without a strong relationship with Bex; trust makes informal help easier.
Existing labels permit deception but analysis can expose it. A legitimate order
does not make controlled chemicals legal.

Samples establish composition. Recorded demands, orders, and witnessed handovers
establish context; a bottle alone cannot establish who abused their authority.
Keep Reyes's cooperation/resistance/complaint history separate from departmental
standing. An upheld complaint produces a substantial caution cooldown rather than
permanently completing a storyline.

Consequences are workplace pressure: specific seizures, inspections, and lost
favors. Ordinary refusal causes no automatic damage, arrest, or detention.
Unresolved incidents cannot accumulate repeated penalties or permanently block
basic chemistry supplies.

## Integration

- Merge Reyes's illicit favors and harassment into one relationship. Retire the
  separate standing-triggered Voss encounters rather than stacking threats.
- Reuse the actual resident, speech, relationships, containers, analysis, and
  evidence handovers. Do not spawn duplicate named characters.
- Store an authority-owned incident record: officer, demand, affected batch,
  witnessed facts, negotiated favor, and resolution. Co-op shares the dispute;
  clients receive observable facts only.
- Provide contextual actions for refusal, negotiation, logged handovers, and
  presenting evidence. Explain consequences through speech and remembered dialogue.
- Inspections visit real places and accessible items. Stored and remote containers
  require explicit searches; remove world-wide silent confiscation.
- Preserve seized contents for recovery and persist incidents and officer history.
  Preserve previously resolved resident stories in old saves.

## First slice and acceptance

Build one disputed seizure with Reyes and Bex before adding requisitions and
protection bargains. An honest player must be able to recover a legitimate batch
without bribery, using both chemical and contextual evidence. Cooperation,
refusal, and appeal must produce understandable, distinct reactions. Verify
spatial inspections, resident identity, recovery, co-op exactly-once resolution,
save/load, and breathing room. Run focused tests, save/wire round trips, workspace
tests, clippy review, and host/client playtests before broadening content.
