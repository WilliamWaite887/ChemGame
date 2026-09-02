# Rogue Security: chemistry under pressure

Status: **Disputed-seizure pilot implemented** (2026-09-02).
Conversation-first intake and physical queues are implemented prerequisites.
Held-item inspection, label-first packaging and physical analyzer reports are
implemented prerequisites; see [label-first-chemistry-plan.md](label-first-chemistry-plan.md).

## Agreed direction

Corruption is occasional systemic workplace trouble alongside the main campaign,
not a new main antagonist or a finite investigation quest. On some saves Officer
Reyes is corrupt, using the existing hidden resident assignment. Warden Bex stays
accountable and provides a dependable appeal route. Honest chemists can encounter
pressure but always have workable options without supplying drugs or accepting
violence. Individual disputes resolve; the relationship continues.

## Pilot and later incidents

- **Unlogged requisition (subsequent work):** supply chemicals, offer a useful legitimate substitute,
  refuse, or insist on an accountable handover. Substitutes must meet the actual
  need; an explicit drug demand does not accept any convenient mixture.
- **Disputed seizure (current pilot):** surrender a particular batch temporarily, negotiate its
  release, or challenge the claim using a sample and the incident record.
- **Protection bargain (subsequent work):** exchange supplies for a concrete favor, such as releasing
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

### Approved disputed-seizure flow

- Begin after three successful ordinary deliveries, every 8–12 minutes of open
  active work, one unresolved case. Resolution restarts the interval; an upheld
  complaint grants 20 minutes. Use the existing hidden Reyes assignment and keep
  Bex accountable, with evidence-based help independent of standing.
- Target a finished, genuinely legal batch on a reachable lab bench for an
  accepted ordinary order with at least 90 seconds left. Exclude reacting,
  emergency/campaign, spare, held, slotted and stored batches. Retain 1u.
  Track fresh completed chemistry against the oldest matching accepted ordinary
  request at production time. Existing stock and analyzer scans never create
  that association; only the first available batch claims a request. Authorized
  chamber transfers and packaging preserve provenance. Chemistry changes or
  request completion invalidate it, and session changes clear it.
- Reyes uses shared greeting intake and existing conversation validation. Opening
  does not seize. Offer grounds, recorded hold, refusal and departure. Unheard
  corruption approaches have no ordinary standing penalty. Refusal can announce
  a 45-second inspection notice, never automatic violence/arrest/detention.
- Physically reach the same unchanged batch or withdraw. Record order, grounds,
  identity and seizure. Give a reference sample and preserve the original
  container/remainder in a Security evidence locker. Pause only the affected
  order, starting at actual seizure; resume when released batch is collected.
- Analyze sample, print report, visit Bex at Security and explicitly present
  evidence. Verify authentic sample/report connection plus contextual case facts.
  Labels and unrelated reports cannot establish the case. Upheld complaints
  release custody, record caution and start the cooldown.
- Restore the unchanged returned sample without duplication. Lost reports can
  be reprinted; Bex can examine custody if the sample is lost/altered. Missing
  liquid is never recreated. Replacement delivery detaches the order. Abandon
  claim forfeits custody and resumes an outstanding order.
- Show hold on the order and a compact case card on the existing board. Reuse
  residents and shared motion. Keep Bex available; interrupted/incapacitated
  actors must leave an accessible recovery path. Never confiscate globally.
- Persist case/custody/evidence/history with stable IDs, but do not restore old
  customer orders or attach their hold to another session's order. Retire Voss
  and merge Reyes corruption without deleting rewards or resolved stories.

Build one disputed seizure with Reyes and Bex before adding requisitions and
protection bargains. An honest player must be able to recover a legitimate batch
without bribery, using both chemical and contextual evidence. Cooperation,
refusal, and appeal must produce understandable, distinct reactions. Verify
spatial inspections, resident identity, recovery, co-op exactly-once resolution,
save/load, and breathing room. Run focused tests, save/wire round trips, workspace
tests, clippy review, and host/client playtests before broadening content.
