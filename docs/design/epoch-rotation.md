# Design: Epoch rotation

Status: draft (Consensus). Code state: `origin/main` 8170a82.

## 1. Goal

Make the committee epoch real. Today every committee is epoch 0
(`kvnc-node/src/main.rs` builds `CommitteeInfo::try_new(0, ..)`), so the
`epoch` field of the v1 vote signing context (`SigningContext { chain_id, epoch }`,
`docs/SIGNATURE_FORMAT.md`) never changes. We want:

- a deterministic epoch boundary that every honest node agrees on;
- committee (and stake) changes that take effect only at that boundary;
- votes and signatures from the previous epoch handled explicitly (accepted only
  where the old committee is still authoritative, rejected everywhere else);
- **Consensus as the single source of the current epoch** for the node and for
  Network (gossip edge validation).

Non-goals: staking/reward logic for choosing the next committee (Execution),
state sync across epochs (`docs/STATE-SYNC.md`), key rotation inside an epoch.

## 2. Current state (grounded in code)

| Item | Where | Today |
|---|---|---|
| Committee + epoch | `kvnc-consensus/src/types.rs` `CommitteeInfo { epoch, authorities, .. }`, `epoch()` | set once at startup, always 0 |
| Vote ctx | `kvnc-consensus/src/engine.rs` `vote_signing_ctx()` | `chain_id` from local ctx + `self.committee.epoch()`; never from the message |
| Vote bytes | `kvnc-types/src/vote.rs` `Vote::signature_data(ctx)` | 74 bytes, `KUNA/vote/v1` + chain_id + epoch + voter + leader_round + leader_hash. `Vote` itself has **no epoch field** |
| Tx ctx | `kvnc-types/src/transaction.rs` | `chain_id` only; epoch is NOT part of tx hash (test `epoch_is_not_part_of_tx_hash`) |
| Node ctx | `kvnc-node/src/main.rs:447` | `SigningContext::new(chain_id).with_epoch(committee.epoch())` |
| Network edge | `kvnc-network/src/validation.rs` `VoteVerifier`, `AuthorityKeys` | one static key map, ctx injected at startup |
| Leader schedule | `CommitteeInfo::leader(round)`, engine `leader_schedule` | round-robin over one committee |
| Rounds | `kvnc-types` `WAVE_LENGTH = 3`, leader/vote/decide offsets | global round counter, not reset per epoch |

Consequence: the engine holds one immutable `CommitteeInfo`; there is no API to
replace it, and Network has its own copy of the key map.

## 3. Model

### 3.1 Epoch boundary

- Epoch `e` covers a contiguous range of **leader waves**. The boundary is
  defined by consensus output, not wall-clock: epoch `e` ends at the first
  committed leader whose round `>= end_round(e)`, where
  `end_round(e) = start_round(e) + EPOCH_LENGTH_ROUNDS` (a multiple of
  `WAVE_LENGTH`; proposed constant in `kvnc-types`).
- The **closing commit** is the `CommittedSubDag` that crosses `end_round(e)`.
  It is delivered in epoch `e` and executed under epoch `e` rules.
- The next committee `C(e+1)` must be fixed by state that is final **before**
  the closing commit (e.g. computed by Execution/staking at a commit at least one
  wave earlier and recorded in the committed state). Consensus only consumes it.
- `start_round(e+1) = leader_round(closing commit) + WAVE_LENGTH` (the next
  leader round). Rounds are not reset; the global round counter keeps increasing,
  so `(round)` alone still identifies a slot and existing indices
  (`decided`, `prune_boundary`) need no epoch dimension.

### 3.2 Source of truth

`CommitteeInfo` becomes epoch-indexed inside Consensus:

```text
EpochSchedule { epochs: BTreeMap<Epoch, (start_round, CommitteeInfo)> }
committee_for_round(r) -> &CommitteeInfo   // used for leader, quorum, keys
current_epoch()        -> Epoch            // used for own votes/blocks
```

- The node and Network never compute the epoch themselves; they subscribe to an
  `EpochChanged { epoch, start_round, committee }` event emitted by the engine
  after the closing commit has been durably marked
  (`mark_decided_and_commit_leader`), so restarts replay the same event from
  durable state.
- `vote_signing_ctx()` changes from "local committee epoch" to
  "epoch of `committee_for_round(vote.leader_round)`", still computed only from
  local state (never from the message), satisfying the v1 rule.

### 3.3 Votes/signatures across the boundary

`Vote` carries no epoch, so the verifier derives it: `epoch = epoch_of(leader_round)`.

| Vote for leader round in | Accepted? | Key set / ctx |
|---|---|---|
| current epoch `e` | yes | `C(e)`, ctx epoch `e` |
| previous epoch `e-1`, leader round `<= end_round(e-1)` and not yet decided locally | yes (late votes can still finish the closing wave) | `C(e-1)`, ctx epoch `e-1` |
| `e-1` but already decided / below `last_decided_round` | dropped (no effect, no penalty) | — |
| `<= e-2` | rejected | — |
| future epoch (start not yet known locally) | buffered, bounded, re-verified when `EpochChanged` arrives; else dropped | — |

A vote signed with the wrong epoch for its round never verifies, because the
epoch is in the signed bytes. That is the intended replay protection: a vote for
round `r` cannot be replayed into another epoch.

Blocks: block signatures today sign the digest (`block_manager.rs` `crypto::sign(&signing_key, digest)`);
`signing_hash(&ctx)` for blocks commits to `chain_id`. Block author keys are taken
from `committee_for_round(block.round)`. A block from an author not in that
committee is rejected by `validate_block` exactly as an unknown authority today.

Transactions: unchanged (epoch not in tx hash), so mempool content survives a
rotation.

### 3.4 Quorum across the boundary

All quorum checks (`quorum_threshold`, `validity_threshold`, commit rule in
`committer.rs`) use `committee_for_round(leader_round)`. A wave never mixes two
committees: the boundary is at a leader round, and the vote/decide rounds of a
wave belong to the leader's epoch.

## 4. Interface changes per crate

- **kvnc-types (Foundation)**: `EPOCH_LENGTH_ROUNDS` constant; optional
  `EpochChange` type for the event payload. No change to `Vote` bytes or v1 format.
- **kvnc-consensus (Consensus)**: `EpochSchedule`; `ConsensusEngine` stores it
  instead of a single `committee`; `committee_for_round`, `current_epoch`;
  `vote_signing_ctx(round)`; epoch event channel; `LeaderSchedule` per epoch;
  committer quorum lookup per round; remove the `TODO(owner)` at
  `engine.rs:240` by deriving epoch from the schedule.
- **kvnc-dag (Consensus)**: `BlockManager` key map becomes per-epoch
  (`authority_keys_for_round`); `select_parents` filters by the committee of the
  parent round; persist the epoch schedule in the same redb transaction as the
  closing commit (new table `DAG_EPOCHS`) so it survives restart.
- **kvnc-network (Network)**: `AuthorityKeys` + `VoteVerifier` take the epoch
  from the Consensus event; verify `vote` against `(C(epoch_of(round)), ctx epoch)`;
  keep `C(e-1)` until `end_round(e-1)` is decided.
- **kvnc-node (Network)**: wire the event; stop constructing ctx from a static
  committee (`main.rs:447`); build initial schedule from genesis + durable store.
- **kvnc-staking / execution (Execution)**: produce `C(e+1)` deterministically
  from committed state at least one wave before the boundary.
- **kvnc-mempool (Network)**: none (tx ctx has no epoch); remove the
  `TODO(owner)` epoch note only.

## 5. Migration

1. Ship `EpochSchedule` with a single entry `(0, start_round 0, genesis committee)`.
   Behaviour is identical to today (all signatures still epoch 0). Tests stay green.
2. Add durable `DAG_EPOCHS` with `#[serde(default)]`-style back-compat: a DB without
   the table is read as epoch 0 only.
3. Wire Network to the epoch event (still only epoch 0).
4. Enable rotation behind a genesis/config flag with `EPOCH_LENGTH_ROUNDS`; testnet first.
5. Only then let Execution feed real committee changes.

No change to the v1 byte format, so no new signature version is needed.

## 6. Risks

- **Non-deterministic boundary**: if `C(e+1)` depends on anything not final
  before the closing commit, nodes diverge. Mitigation: one-wave lag rule.
- **Liveness at the boundary**: old committee must finish the closing wave; if
  `C(e-1)` keys are dropped too early, the last wave stalls. Mitigation: keep
  `C(e-1)` until `end_round(e-1)` decided.
- **Crash between commit and epoch persistence**: must be one redb transaction
  (same lesson as #14 atomic commit).
- **Leader schedule churn**: `leader_schedule` overrides keyed by round must not
  leak across epochs.
- **Pruning**: `prune_waves_before` must not remove the closing wave before the
  next epoch's first commit (recovery needs it).
- **Network/Consensus disagreement** while the event is in flight: Network may
  briefly reject valid new-epoch votes; buffer is bounded so this costs latency,
  not safety.

## 7. Test plan

- Unit (consensus): `committee_for_round` across a boundary; `vote_signing_ctx`
  derives epoch from round; vote for `e-1` accepted only while undecided; `e-2`
  rejected; vote with correct key but wrong epoch fails (extends `sig_v1_*`).
- Unit (dag): durable `DAG_EPOCHS` survives reopen; closing commit and epoch row
  are atomic (fault injection like `dag_store::fault`).
- Property: random epoch lengths, no commit uses quorum of the wrong committee.
- Integration (node, Network-owned): 4 nodes, rotate to a different committee at
  round N, liveness continues, same committed leaders on all nodes
  (`sustained_liveness_integration` variant), restart in the middle of a boundary.
