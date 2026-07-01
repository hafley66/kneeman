# Distributed State Consistency for Multiplayer Games

*A rigorous survey mapped onto: Godot client + Rust core, a rollback fighter today, a persistent shared "world" later, possibly p2p, self-hosted on one VPS.*

---

## 0. The question, answered first

> "If peers sync state via timestamps, is that Raft?"

**No. It is the opposite of Raft.** These two ideas sit at opposite corners of the CAP tradeoff, and conflating them leads to building the wrong thing.

| | "Sync via timestamps" | Raft |
|---|---|---|
| What it is | A **Last-Writer-Wins (LWW) register** — a CRDT | A **consensus algorithm** over a replicated log |
| Consistency | **Eventual / AP** (available under partition) | **Strong / CP** (consistent, blocks under partition) |
| Leader? | None. Every peer writes locally, no coordination | **One elected leader**; all writes funnel through it |
| Agreement | None. Conflicts resolved by *discarding* the loser | Total order agreed by a **majority quorum** before commit |
| Concurrent writes | Silently drops one edit (higher timestamp wins) | No silent loss; every entry has one agreed log position |

Attaching a timestamp to each update and keeping the "latest" is textbook **LWW-register**, one of the original CRDTs, delivering *eventual consistency* (AP). Raft is **CP**: it elects a single leader and gets a majority quorum to agree on **one totally-ordered log** before anything commits. Timestamp-sync has no leader, no quorum, no agreement, no total order. It cannot be Raft, and it has a specific trap (§2.4).

---

## 1. Consensus algorithms: Raft, Paxos, Multi-Paxos

### 1.1 What they actually solve

One problem: **get unreliable machines to agree on a single totally-ordered sequence of commands (a replicated log), so every machine applies the same commands in the same order and ends in the same state** — the replicated state machine pattern.

Raft does it by (1) **electing a leader** (terms, candidates, majority vote) and (2) **funneling all writes through the leader**, which marks an entry **committed** only once a majority has stored it. Paxos solves single-value agreement; **Multi-Paxos** chains it to a log. Raft (2014) was designed to be *understandable* while being equivalent and as efficient as Multi-Paxos, which is why it displaced Paxos in most new systems.

### 1.2 CP in CAP terms

The **CAP theorem**: under a network partition you get only two of {Consistency, Availability, Partition-tolerance}. Consensus chooses **CP** — during a partition it **refuses to make progress** rather than serve inconsistent data. Raft commits only with a majority quorum, so a minority partition stalls. ZooKeeper, Spanner, etcd are all consensus-backed CP systems. "Refuse to make progress" is exactly what you don't want on a gameplay hot path.

### 1.3 Membership changes are expensive

Changing the server set risks split-brain (two disjoint majorities). Raft offers **joint consensus** (transition through a combined `C-old,new` where both majorities must agree) or **single-server changes** (add/remove one node at a time). Both are careful, rare operations with operational windows. **Games change membership every few seconds, uncoordinated** — the core structural mismatch with consensus.

### 1.4 Why games (almost) never use consensus for *gameplay*

1. **Quorum stalls kill feel** — CP blocks under partition/latency; a fighter needs a frame in 16.6 ms.
2. **Churn = constant reconfiguration** — join/leave maps onto Raft's most expensive op.
3. **Wrong consistency target** — gameplay wants determinism (§3) or authoritative truth (§4), not peer voting.
4. **N is tiny and untrusted** — Raft assumes 3/5 *trusted* crash-fault nodes, not 2–8 mutually-untrusting cheating clients (Byzantine).

### 1.5 Where Raft *does* show up — game **infra**, not gameplay

- **etcd** (Raft, powers Kubernetes) and **Consul** (Raft) for service discovery, config, coordination.
- **Matchmaking/session coordination** — a distributed lock so two matchmakers don't double-book a server.
- **Durable economy ledgers / leaderboards** on a CP store when you need linearizable no-lost-write semantics.

> **→ Applies to this project.** You are on **one VPS** — one machine, no cluster, no quorum. Raft is irrelevant to gameplay in every mode. If you ever scale to multiple backend nodes, use **etcd/Consul off the shelf** for coordination. Never write Raft for gameplay state.

---

## 2. CRDTs and logical clocks

### 2.1 What a CRDT is

A **Conflict-free Replicated Data Type**: a replicated structure where any replica updates independently without coordination, and a merge function guarantees all replicas **eventually converge**. Defined 2011 (Shapiro, Preguiça, Baquero, Zawirski), motivated by collaborative editing. An **AP** design.

### 2.2 State-based vs op-based vs delta

- **State-based (CvRDT)** — ship whole state, merge via a semilattice join (commutative, associative, idempotent). Robust over lossy channels; bandwidth-heavy.
- **Op-based (CmRDT)** — broadcast commutative operations; needs reliable causal delivery. Small messages.
- **Delta-state (δ-CRDT)** — ship small delta-mutations, reconciled by anti-entropy. Modern middle ground (low bandwidth + robustness).

### 2.3 The catalog

| CRDT | Semantics |
|---|---|
| **G-Counter** | Grow-only; merge = per-replica max, value = sum |
| **PN-Counter** | Inc + dec, as two G-Counters |
| **LWW-Register** | Value + timestamp; **highest timestamp wins** |
| **G-Set / 2P-Set** | Grow-only / tombstoned-remove set |
| **OR-Set** | Observed-Remove: add wins over concurrent remove via unique tags |
| **RGA** | Ordered sequence; unique `(nodeId, seq)` ids give deterministic tie-breaking |
| **Fugue / FugueMax** | Sequence CRDTs guaranteeing maximal non-interleaving (fixes `HWeolrllod` garbage) |

### 2.4 Logical clocks — and the LWW trap

- **Lamport timestamp** — one integer, `max(local,received)+1`. Total order consistent with causality, but **cannot detect concurrency**.
- **Vector clock** — one counter per node, element-wise max. **Detects concurrency**: neither vector dominates → concurrent.
- **Version vector** — same, specialized to replica versions of an item.

The trap: **"sync via timestamps" = LWW-register.** On concurrent edits it keeps the higher timestamp and **silently discards the other**. A vector clock would *detect* the conflict; a bare timestamp just picks a winner. And timestamps assume synchronized clocks — **clock skew means "whoever's clock runs fast" wins**, not whoever acted last. LWW is only safe when concurrent writes are rare or losing one is acceptable.

### 2.5 Real libraries

- **Yjs** (JS, production default) — shared Map/Array/text, auto-merge, offline, snapshots, undo/redo.
- **yrs / y-crdt** — **native Rust port of Yjs**; the relevant one for a Rust core.
- **Automerge** — Rust CRDT, JSON model, Git-like change history, WASM JS bindings.
- **Loro** — newer Rust CRDT (Peritext + Fugue for rich text).

### 2.6 When CRDTs fit a game world — and when they don't

**Fit** (mutable, collaborative, additive, eventual convergence OK): terrain/voxel/tile edits, shared annotations/drawings, sign text, presence, cursors, cosmetic placement.

**Do not fit** (needs a real transaction/invariant): **economy and inventory** ("transfer 1 sword" must be atomic, never dupe/lose), currency balances that mustn't go negative, trades, crafting that consumes inputs. CRDTs have no transaction and will let two peers each keep the sword (dupe) or neither (loss).

> **→ Applies to this project.** For the world, a **CRDT (yrs/Automerge in the Rust core) is legitimate for terrain/decoration** if you want offline edits + p2p convergence. **Route inventory/currency/trades through an authoritative owner (§4), not a CRDT.** And **never bare-timestamp LWW** — use a real CRDT (OR-Set, sequence CRDT) so you at least *detect* conflicts.

---

## 3. Deterministic lockstep — the third model

Neither consensus nor CRDT. Lockstep achieves **strong consistency by construction**: **every peer runs the identical simulation and exchanges only inputs.** Because the sim is fully deterministic, identical inputs yield **bit-for-bit identical state** everywhere — nothing to transmit, merge, or disagree about. Bandwidth scales with **input size, not world size** (Little Big Planet shipped this).

**Hard requirement:** determinism down to the bit ("so exact you could checksum the entire state each frame"). Main obstacle: **floating-point non-determinism** across compilers/OSes/ISAs — fix with fixed-point math, controlled FP flags, or a deterministic math path.

**Rollback (GGPO-style)** makes lockstep playable over latency: predict remote inputs (usually "same as last frame"), simulate immediately, and on misprediction **roll back** to the last good frame, re-apply corrected inputs, fast-forward. Requires **serialize/restore full state on demand**. Result: near-offline responsiveness — why competitive fighters use it.

| | Consensus (Raft) | CRDT | Deterministic lockstep |
|---|---|---|---|
| Agreement | Vote on a shared log | Merge conflicting states | **None needed** — same inputs → same state |
| On the wire | Log entries | State/ops/deltas | **Inputs only** |
| Consistency | Strong (CP) | Eventual (AP) | **Strong, by construction** |
| Cost | Quorum round-trips | Merge metadata, bw ∝ state | **Bit-exact determinism + rollback state I/O** |
| Scaling | ~5 nodes | Many | **~2–4 players** (waits on laggiest input) |

> **→ Applies to this project.** The fighter is **deterministic rollback lockstep, full stop.** Pure deterministic `step(state, inputs) -> state` in the Rust core with cheap serialize/restore; Godot as render/IO shell; fixed-point or locked FP for cross-machine bit-equality. Not consensus, not CRDT.

---

## 4. P2P authority without a dedicated server

### 4.1 Single-host-authority + host migration

One peer is **host**, runs the authoritative sim, relays to others. Cheap to start; but if the host leaves the session collapses unless a new host is promoted — **host migration**, notoriously hard (state transfer, resync, host selection). Industry consensus: the initial P2P savings evaporate against the engineering to build reliable migration, which is why many teams move to authoritative servers that eliminate it.

### 4.2 Leaderless

No owner; peers reconcile via CRDT/lockstep. Works for convergent (terrain via CRDT) or deterministic (lockstep) state, but has **no authority to enforce invariants** — fatal for economy/inventory and anti-cheat.

### 4.3 Anti-cheat: the peer-trust problem

**In P2P, whoever holds authority owns the truth — and a cheating host owns the simulation.** Client-side anti-cheat does nothing about a malicious host. The only structural defense is to **move authority off the players**: a server-authoritative backend owns truth and validates every action, so players can't manipulate state they don't run.

> **→ Applies to this project.** Use the **VPS as authoritative owner for the world's valuable state** (inventory, currency, trades, world commits). That single VPS *is* your server-authoritative backend — it sidesteps host migration and gives anti-cheat a trusted reference. For the fighter, p2p rollback between two players is fine (both simulate deterministically; checksum to detect desync; no economy to protect). Reserve leaderless-CRDT for **cosmetic/terrain** where a cheater merely griefs pixels.

---

## 5. Decision matrix

| Axis | **Raft / consensus** | **CRDT (incl. LWW)** | **Deterministic lockstep** | **Host-authority (server/host)** |
|---|---|---|---|---|
| Consistency | Strong (CP) | Eventual (AP) | Strong by construction | Strong (authority = truth) |
| Leader? | Yes — elected quorum | No | No (symmetric) | Yes — host/server |
| On the wire | Log entries + votes | State/ops/deltas | **Inputs only** | Snapshots/deltas from authority |
| Bandwidth | Moderate (quorum RT) | ∝ state (delta cuts it) | **Tiny** (∝ inputs) | ∝ world state (interest-managed) |
| Churn | **Poor** (reconfig) | **Excellent** | Poor (~2–4 players) | Moderate (server great; host needs migration) |
| Cheat resistance | N/A (trusted nodes) | **None** | Detects desync; no invariants | **Best** (authority validates) |
| Complexity | High | Medium–High | High (determinism + rollback I/O) | Low–Med (server); High (host migration) |
| **Use for** | Backend control plane (etcd/Consul) | Convergent world state: terrain, decorations | Real-time sim: **fighters**, RTS | Authoritative truth: **inventory, economy, trades** |

---

## 6. Recommendations

1. **Fighter (today): deterministic rollback lockstep.** Pure `step()`, fixed-point, cheap serialize/restore; p2p between two players; checksum each frame. Not Raft, not CRDT.
2. **Persistent world (later): authoritative VPS for valuable state.** VPS owns inventory/currency/trades (real transactions + cheat resistance), removing host-migration work.
3. **World terrain/decoration: CRDT optional and legitimate.** For mutable non-valuable state, use a real CRDT via **yrs/Automerge** (OR-Set/sequence, not bare timestamps).
4. **Never bare-timestamp LWW** for anything you can't afford to silently lose.
5. **Raft only if you outgrow one VPS**, and only for backend coordination via etcd/Consul off the shelf.

---

## Sources

- [Ongaro & Ousterhout, *In Search of an Understandable Consensus Algorithm* (Raft), USENIX ATC '14](https://www.usenix.org/conference/atc14/technical-sessions/presentation/ongaro) · [PDF](https://raft.github.io/raft.pdf)
- [Raft Group Reconfiguration / joint consensus (Redpanda)](https://docs.redpanda.com/current/manage/raft-group-reconfiguration/) · [JOINT-CONSENSUS notes](https://github.com/peterbourgon/raft/blob/master/JOINT-CONSENSUS.md)
- [etcd vs Consul (Raft usage)](https://gist.github.com/yurishkuro/10cb2dc42f42a007a8ce0e055ed0d171)
- [CAP Theorem (PingCAP)](https://www.pingcap.com/article/understanding-cap-theorem-basics-in-distributed-systems/) · [CAP (GeeksforGeeks)](https://www.geeksforgeeks.org/system-design/cap-theorem-in-system-design/)
- [CRDT (Wikipedia)](https://en.wikipedia.org/wiki/Conflict-free_replicated_data_type) · [Shapiro et al., CRDTs (SSS 2011, PDF)](https://gsd.di.uminho.pt/members/cbm/members/cbm/ps/sss2011.pdf)
- [Delta-CRDTs (arXiv:1410.2803)](https://arxiv.org/abs/1410.2803) · [Springer](https://link.springer.com/chapter/10.1007/978-3-319-26850-7_5)
- [OpSets / RGA (arXiv:1805.04263)](https://arxiv.org/pdf/1805.04263) · [Fugue (arXiv:2305.00583)](https://arxiv.org/pdf/2305.00583v1)
- [Lamport timestamp (Wikipedia)](https://en.wikipedia.org/wiki/Lamport_timestamp) · [Vector Clocks (SysDesAi)](https://www.sysdesai.com/learn/distributed-systems/vector-clocks)
- [Yjs](https://github.com/yjs/yjs) · [Yrs — Rust port (NLnet)](https://nlnet.nl/project/Yrs/) · [crdt.tech implementations](https://crdt.tech/implementations) · [Automerge 2.0](https://automerge.org/blog/automerge-2/) · [Loro](https://loro.dev/blog/crdt-richtext) · [Yjs vs Automerge vs Loro 2026](https://www.pkgpulse.com/guides/yjs-vs-automerge-vs-loro-crdt-libraries-2026)
- [Deterministic Lockstep (Gaffer)](https://gafferongames.com/post/deterministic_lockstep/) · [Floating Point Determinism (Gaffer)](https://gafferongames.com/post/floating_point_determinism/)
- [Netcode Architectures Part 2: Rollback (SnapNet)](https://www.snapnet.dev/blog/netcode-architectures-part-2-rollback/) · [GGPO](https://www.ggpo.net/) · [GGPO (Wikipedia)](https://en.wikipedia.org/wiki/GGPO)
- [Host Migration (Edgegap)](https://edgegap.com/blog/host-migration-in-peer-to-peer-or-relay-based-multiplayer-games) · [Authoritative vs P2P (Rivet)](https://rivet.gg/docs/general/concepts/authoritative-vs-p2p)
- [Server-Authoritative Anti-Cheat (2026)](https://crux.supercraft.host/blog/server-authoritative-anti-cheat-backend/) · [Cheaters & P2P (Edgegap)](https://edgegap.com/blog/cheaters-peer-to-peer-hosting-an-beginners-guide)
