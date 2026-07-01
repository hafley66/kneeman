# Datastore Architecture for Video Games

A field guide to how games — indie flat-file to AAA planet-scale — store state, and where a
single-VPS cozy-hangout game (Godot client, Rust core, Postgres backing store, `ggrs` sim in RAM,
HTTP-served assets) fits. Every claim is cited. Facts verified July 2026 against live docs.

> **→ Applies to this project (TL;DR).** Your instincts are right for your scale. Realtime sim
> belongs in RAM (`ggrs`), never a DB. Postgres is the correct single system-of-record for accounts,
> inventory, and currency. Assets belong behind HTTP/object storage, not in Postgres rows. You do
> **not** need Redis, a read replica, a warehouse, or Cassandra/Scylla yet — those are load-triggered
> additions, and the triggers are quantified in the last section. The one thing you *cannot* defer is
> getting the economy tier correct (idempotency + ledger), because dupes are unrecoverable.

---

## 1. The tiered model

Games do not use "a database." They use up to five stores, each chosen because its
latency/durability/consistency profile is different. Mixing tiers (e.g. per-tick sim state in
Postgres) is the classic mistake — the whole point is that these needs *disagree*.

| Tier | Latency need | Durability need | Consistency need | Conventional winner |
|---|---|---|---|---|
| 1. Realtime session state | Sub-frame (µs, in-process RAM) | None (ephemeral) | Strong / deterministic / authoritative | Authoritative game-server process + rollback lib (GGPO / **ggrs**) |
| 2. Cache / presence / matchmaking / leaderboards | Sub-millisecond | Secondary / optional | Single-node atomic | **Redis** (sorted sets, hashes, TTL, Pub/Sub) |
| 3. System-of-record | Low, interactive txn | Maximum (crash-safe) | Strong ACID | **Postgres** / MySQL / Aurora |
| 4. Analytics / telemetry firehose | OLAP interactive → batch; ingest in seconds | High, loss-tolerant, append-only | Eventual | ClickHouse / BigQuery / Snowflake / S3+Parquet, fed by Kafka/Kinesis |
| 5. Assets / blobs | Edge-low-latency, high throughput, byte-range | 11 nines | Immutable / eventual | S3 / GCS + CDN |

### Tier 1 — Realtime session state (RAM, not a DB)

Live simulation state lives in the game-server process. The canonical treatment is Glenn Fiedler's
[Networked Physics series](https://gafferongames.com/categories/networked-physics/), which networks a
physics sim three ways:
[deterministic lockstep](https://gafferongames.com/post/deterministic_lockstep/),
[snapshot interpolation](https://gafferongames.com/post/snapshot_interpolation/), and
[state synchronization](https://gafferongames.com/post/state_synchronization/).

Rollback netcode is the reason this tier can never be a DB. [GGPO](https://www.ggpo.net/) advances
local game logic immediately and *predicts* remote inputs, then rolls back and re-simulates when the
real inputs arrive; integrating it requires only "save state, load state, and execute one frame
without rendering"
([GGPO source, MIT, open-sourced Oct 2019](https://github.com/pond3r/ggpo)). Your stack uses
[**ggrs**](https://github.com/gschup/ggrs), the Rust P2P reimplementation of GGPO.

Determinism is what forbids a DB in the loop: given identical initial state and inputs, the sim must
produce a bit-identical result so any peer can replay it
([deterministic lockstep](https://gafferongames.com/post/deterministic_lockstep/)). A mutable DB row,
a network round-trip, and non-deterministic read timing all break that. The sim must save → roll back
→ re-simulate inside one frame budget; a DB call does not fit. And the state is *discarded* when the
match ends — durability is a non-goal.

> **→ Applies to this project.** `ggrs` holds the match in RAM and is authoritative for the match.
> Postgres never sees a per-tick write. What you persist is the *outcome* of a match (result, XP,
> currency delta), written once at match end as a single transaction — see Tier 3 and §4.

### Tier 2 — Cache / presence / matchmaking / leaderboards (Redis)

Redis wins on sub-millisecond in-memory ops with the right built-in types:

- **Leaderboards → Sorted Sets (ZSET).** Redis's own docs name this exact use case: "you can use
  sorted sets to easily maintain ordered lists of the highest scores in a massive online game"
  ([Sorted sets](https://redis.io/docs/latest/develop/data-types/sorted-sets/)). `ZADD` is O(log N),
  range/rank reads are O(log N (+M)), atomic bumps via `ZINCRBY`
  ([leaderboard tutorial](https://redis.io/tutorials/howtos/leaderboard/),
  [ZADD](https://redis.io/docs/latest/commands/zadd/)).
- **Presence / sessions / rate limiting** ([use-cases](https://redis.io/docs/latest/develop/use-cases/),
  [rate limiting](https://redis.io/glossary/rate-limiting/)).
- **Matchmaking + session state**: queued players in a ZSET keyed by mode+skill bucket, player meta in
  hashes, rooms as JSON with a TTL so stale rooms self-clean, atomic join via `WATCH`/`MULTI`
  ([Redis matchmaking tutorial](https://redis.io/tutorials/matchmaking-and-game-session-state-with-redis/)).

Durability is deliberately secondary — Redis offers RDB snapshots, per-write AOF, both, or none;
"you can disable persistence completely… used when caching," and AOF-every-second means "you can only
lose one second worth of writes"
([persistence](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/)).

> **→ Applies to this project.** You do not have Redis and do not need it yet (see §5). A `ZSET`
> leaderboard and Redis matchmaking are the *first* thing to add when a single Postgres starts eating
> hot small reads/writes, or when you want a global leaderboard that isn't a `SELECT … ORDER BY` scan.

### Tier 3 — System-of-record durable data (relational)

Accounts, inventory, economy ledgers, purchases, entitlements. Postgres / MySQL / [Amazon
Aurora](https://docs.aws.amazon.com/AmazonRDS/latest/AuroraUserGuide/CHAP_AuroraOverview.html)
(Postgres/MySQL-compatible). The whole argument is ACID transactions, which Postgres's own tutorial
makes with the money example: a transaction is "atomic: from the point of view of other transactions,
it either happens completely or not at all," it is durable ("logged in permanent storage… before the
transaction is reported complete"), and "it would certainly not do for a system failure to result in
Bob receiving \$100.00 that was not debited from Alice"
([transactions](https://www.postgresql.org/docs/current/tutorial-transactions.html)).

> **→ Applies to this project.** This is your Postgres. It is the correct, boring, right answer for a
> cozy-hangout game. §3 covers how to not blow your foot off with it; §4 covers the economy rules you
> can't get wrong.

### Tier 4 — Analytics / telemetry firehose

Events are append-only and queries touch few columns over billions of rows, so columnar OLAP wins:

- **ClickHouse** — "analytical queries that only need a few columns… scan only relevant data,"
  observability data stored "as wide, rich events"
  ([ClickStack overview](https://clickhouse.com/docs/use-cases/observability/clickstack/overview)).
- **Google BigQuery** — Google's own mobile-game pipeline is client → Pub/Sub → Dataflow → BigQuery
  ([build a mobile gaming analytics platform](https://cloud.google.com/blog/products/gcp/build-a-mobile-gaming-analytics-platform));
  Firebase exports daily `events_YYYYMMDD` + streaming `events_intraday_` tables
  ([BigQuery export](https://firebase.google.com/docs/projects/bigquery-export)).
- **Snowflake / S3 + Parquet** — immutable compressed columnar micro-partitions, storage/compute split
  ([Snowflake key concepts](https://docs.snowflake.com/en/user-guide/intro-key-concepts));
  [Apache Parquet](https://parquet.apache.org/docs/) as the open lake format.
- **Kafka / Kinesis** as the ingest firehose — Kafka "latencies as low as 2ms," "trillions of messages
  per day" ([Kafka intro](https://kafka.apache.org/intro/)); Kinesis "in real time at any scale,"
  24h default retention
  ([Kinesis](https://docs.aws.amazon.com/streams/latest/dev/introduction.html)).

Consistency is intentionally weaker than Tier 3: ordered, durable, append-only, eventually visible
downstream. No cross-key ACID.

> **→ Applies to this project.** At your scale a warehouse is overkill. Your existing event firehose
> (the relay `/ev` sink noted in the repo history) *is* the poor-man's Tier 4: append-only NDJSON with
> rotation. When "grep the event log" stops answering product questions, the honest next step is a
> single local **ClickHouse** or even DuckDB-over-Parquet on the same box — not BigQuery/Snowflake.

### Tier 5 — Assets / blobs (object storage + CDN)

Large immutable binaries do not belong in a DB. Object stores give 11-nines durability cheaply, a CDN
caches bytes at the edge near players, and byte-range HTTP lets a patcher fetch only changed ranges.

- [Amazon S3](https://docs.aws.amazon.com/AmazonS3/latest/userguide/DataDurability.html): "99.999999999%
  (11 nines) durability… across a minimum of three Availability Zones."
- [Google Cloud Storage](https://cloud.google.com/storage/docs/availability-durability): "at least 11
  9's annual durability… erasure coding."
- [Amazon CloudFront](https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/HowCloudFrontWorks.html):
  CDN with 750+ POPs that "reduces the load on your origin server and reduces latency";
  [Google Cloud CDN](https://cloud.google.com/cdn/docs/overview) is the counterpart.

> **→ Applies to this project.** You already serve assets over HTTP through nginx — that is Tier 5 done
> correctly for a single VPS. nginx is your origin and static file server; when download volume or
> geographic spread hurts, put a CDN (Cloudflare/CloudFront) *in front of the same nginx origin* rather
> than moving bytes into Postgres. Never store character scans, gifs, or patch blobs as `bytea`.

---

## 2. Real shipped-game architectures

Every claim cited. Uncertain / secondary-sourced claims flagged inline.

### EVE Online (CCP) — one Microsoft SQL Server for the whole universe

One central MS SQL Server backs the single "Tranquility" shard — one universe, so one authoritative
DB. Three tiers: load balancers → PROXY blades → SOL blades (game logic) → central SQL Server; ~90–100
SOL blades, one solar system = one single-threaded process running the *Destiny* physics loop
([High Scalability](https://highscalability.com/eve-online-architecture/),
[EVE Univ Wiki — server tick](https://wiki.eveuniversity.org/Server_tick)). Sim tick is **1 Hz**
([Imperium News](https://imperium.news/understanding-eve-online-server-tick/)). Logic runs in
[Stackless Python](https://talkpython.fm/episodes/show/52/eve-online-mmo-game-powered-by-python).
[Time Dilation](https://www.eveonline.com/news/view/introducing-time-dilation-tidi) slows the game
clock (floor 10% real-time) so an overloaded node keeps its tasklet queue small. CCP has always run
the hot DB on RAM/flash: a 2009 all-SSD RamSan, and by 2020 a box with **4 TB RAM** and NVMe on a
FlashSystem SAN
([History of EVE DB server hardware](https://www.eveonline.com/news/view/a-history-of-eve-database-server-hardware)).
*Flag:* "single un-clustered SQL Server 2008 instance" is HN/forum-level
([HN](https://news.ycombinator.com/item?id=3913062)); CCP confirms one hot DB, not the exact topology.

### Discord — MongoDB → Cassandra → ScyllaDB

Message history is NoSQL wide-column, now ScyllaDB. MongoDB (2015) fell over at ~100M messages when
data+index no longer fit in RAM. Cassandra (2017→2022) grew from 12 to **177 nodes** ("trillions of
messages"), partitioned by (channel_id, time bucket); it suffered hot-partition latency and JVM GC
pauses. ScyllaDB (C++, no GC, shard-per-core) cut the largest cluster to **72 nodes**, read p99 from
40–125 ms to **15 ms**, and insert p99 to a steady **5 ms**; the migration ran at **3.2M records/sec**
and finished in **9 days**
([How Discord Stores Trillions of Messages](https://discord.com/blog/how-discord-stores-trillions-of-messages),
[InfoQ](https://www.infoq.com/news/2023/06/discord-cassandra-scylladb/)).

### Minecraft — flat files (Java) / LevelDB (Bedrock); Realms hosted

No DB server locally. Java uses **Region/Anvil** `.mca` files — 32×32 = 1,024 chunks per file, 8 KiB
header, Zlib-compressed chunk payloads
([Region file format](https://minecraft.wiki/w/Region_file_format),
[Anvil](https://minecraft.wiki/w/Anvil_file_format)). Bedrock uses a **Mojang fork of Google
LevelDB** in the world's `db/` dir, keyed by little-endian x,z + record tag
([Bedrock level format](https://minecraft.wiki/w/Bedrock_Edition_level_format)). Both use
[NBT](https://minecraft.wiki/w/NBT_format) as the tree serialization. **Realms** is one cloud VM per
world; Mojang
[migrated AWS → Azure in 2020](https://developer.microsoft.com/en-us/games/articles/2020/10/migrating-minecraft-realms-from-aws-to-azure/)
(was EC2 + RDS MySQL + S3; now PlayFab + Azure Database for MySQL + Blob Storage).

### World of Warcraft / classic MMO — many realms + relational

A realm is a copy of the world; players bind to one. Per-account character state lives in a relational
DB that also feeds the web Armory
([HowStuffWorks](https://electronics.howstuffworks.com/world-of-warcraft.htm)). Population tech evolved
sharding → phasing → cross-realm zones, with Classic using layering
([Technology.org](https://www.technology.org/2023/11/28/key-technologies-used-in-world-of-warcraft/)).
GDC 2013's
["Network Serialization and Routing in WoW"](https://gdcvault.com/play/1017733/Network-Serialization-and-Routing-in)
describes JAM, the inter-server routing layer. This is the *opposite* trade-off from EVE: many shards
instead of one. *Flag:* Blizzard's exact DB product is undisclosed; the "MySQL" association comes from
the MaNGOS/TrinityCore *emulators*, not Blizzard.

### Supercell (Clash of Clans / Royale) — MySQL → Aurora (state); DynamoDB/Scylla for other jobs

Per-player state is relational: famously "3 server devs run 2,000+ EC2 instances and ~300 MySQL
databases," later migrated to **Amazon Aurora** (MySQL-compatible)
([AWS Innovators](https://aws.amazon.com/solutions/case-studies/innovators/supercell/),
[Aurora case study](https://aws.amazon.com/solutions/case-studies/supercell-aurora-case-study/)).
**DynamoDB is the analytics path** (Kinesis processes up to **45B events/day** into DynamoDB), and a
newer **ScyllaDB** service handles real-time persisted events — chat, presence, friend graph,
progression
([ScyllaDB — Supercell](https://www.scylladb.com/2025/01/14/how-supercell-handles-real-time-persisted-events-with-scylladb/)).
*Flag:* no "one DB per player" — it's shard-per-many-players across ~300 DBs; sharding key
undocumented.

### Fortnite / Epic — MongoDB (state), DynamoDB (analytics), all-in on AWS

The MCP (Matchmaking Control Plane) runs on **9 MongoDB shards** (8 user data + 1 matchmaking), each a
writer + 2 read replicas + hidden; peak **124K client req/s → 318K DB reads/s + 132K writes/s at
sub-10ms avg**. In the 3.4M-CCU outage, matchmaking-shard write times spiked past **40,000 ms**
([Epic postmortem](https://www.fortnite.com/news/postmortem-of-service-outage-at-3-4m-ccu), relayed via
[ByteSizedDesign](https://read.bytesizeddesign.com/p/how-a-34-million-concurrent-users) — the Epic URL
returns 403). Real-time analytics = **Spark + DynamoDB (temp)**; ~2 PB/month, auto-scales ~30× at peak
([AWS — Epic all-in](https://aws.amazon.com/blogs/gametech/epic-fortnite-all-in-on-aws-cloud/)).
[MongoDB names Fortnite](https://www.mongodb.com/blog/post/ten-years-mongodb-customers-aws-reinvent) as
a customer. *Flag:* MCP numbers are Epic's own but retrieved via relay.

### Roblox — HashiStack on bare metal; DataStore for devs; the 2021 Consul outage

Stateful services (CockroachDB, MongoDB, InfluxDB, Elastic) run in containers on bare metal,
orchestrated by Nomad/Consul/Vault
([Portworx interview](https://portworx.com/blog/architects-corner-roblox-runs-platform-70-million-gamers-hashicorp-nomad/)).
The definitive lesson is the
[Oct 2021 73-hour outage postmortem](https://blog.roblox.com/2022/01/roblox-return-to-service-10-28-10-31-2021/):
Consul's *streaming* feature caused Go-channel contention under high read+write, KV p50 write latency
went &lt;300 ms → ~2,000 ms, compounded by a **BoltDB freelist** pathology (a 4.2 GB log store holding
489 MB of real data). Consul was a single point of failure because Nomad and Vault depend on it. The
takeaway for self-hosters: once you run your own stateful services, the datastore risk migrates to the
*coordination layer*. Developer persistence is the
[DataStoreService](https://create.roblox.com/docs/cloud-services/data-stores) API; its backing store is
undocumented.

### RuneScape / OSRS (Jagex) — regional VM worlds + AWS save store

A world is a shard (RS3 ~137 worlds; OSRS up to 2,000 players/world at 0.6 s ticks). Character data
saves to a **global account profile** on logoff and restores on any world at login. Modern topology is
two-tier: a **Core Tier on AWS** (Jagex Accounts, Player Saves, analytics) and an **Edge Tier** of
regional game worlds as Linux VMs on VMware vSphere, bursting into AWS over Direct Connect
([OSRS dev blog](https://oldschool.runescape.wiki/w/Update:More_Worlds,_More_Power:_The_Road_To_Greater_Capacity),
[runescape.wiki/Server](https://runescape.wiki/w/Server)). *Flag:* the original 2004 per-character
`.dat` binary format is RSPS-emulator reconstruction
([rune-server](https://rune-server.org/threads/how-do-i-read-and-edit-dat-player-saves.325049/)), not
Jagex-confirmed.

### Habbo Hotel / Club Penguin — relational DB behind Java socket servers

Habbo's official 2008 architecture doc describes **Java/JVM app servers using Hibernate ORM + Spring**,
a Hibernate second-level cache with cross-JVM revoke messages and **per-row version numbers
(optimistic locking)**, and that "the database has always been a major bottleneck," scaled via
distributed caching + DB replication
([Habbo architecture PDF](https://h4bbo.net/archive/Habbo%20Monster%20Archive%20!%20-%20by%20Wendigo/Others/habboarchitecture.pdf)).
Club Penguin ran on **SmartFoxServer** (commercial Java socket server) with a Flash client
([CP Wiki](https://clubpenguin.fandom.com/wiki/Server)). *Flag:* exact RDBMS product not pinned in
either official source; MySQL confirmed only in the private-server ecosystems.

### Indie — local serialized files, no DB server

The through-line for single-player / small co-op: serialize whole game state to local files. None ship
an embedded DB for saves (Valheim's `.db` extension is misleading — it is custom binary, not SQLite).

| Game | Save format | DB server? | Source |
|---|---|---|---|
| Valheim | custom binary `.db` (world) + `.fwl` (header), ZPackage/ZDO; **not** SQLite | No | [xgamingserver](https://xgamingserver.com/docs/valheim/world-save-files) |
| Terraria | binary `.wld` + `.plr`, 7-byte "relogic" magic | No | [seancode terrafirma](https://seancode.com/terrafirma/world.html) |
| Factorio | `.zip` of serialized C structs (`level.dat` + zlib chunks); blueprints = JSON→zlib→base64 | No | [Factorio Wiki](https://wiki.factorio.com/Blueprint_string_format) |
| Stardew Valley | XML per-farm folder (main save + `SaveGameInfo`) | No | [Steam guide](https://steamcommunity.com/app/413150/discussions/0/405692758715239357) |

### Which real games use what

| Game / system | Primary state store | Relational / NoSQL / flat | Why that choice |
|---|---|---|---|
| EVE Online | one MS SQL Server (Tranquility) | Relational (single-shard) | One universe → one authoritative DB; hot DB on RAM/flash |
| WoW / classic MMO | per-account relational DB, many realms | Relational (many-shard) | Population won't fit one world; realm = copy |
| Discord (messages) | ScyllaDB (was Mongo→Cassandra) | NoSQL wide-column | Trillions of msgs; no-GC C++ fixed tail latency |
| Fortnite / Epic (state) | MongoDB (MCP, 9 shards) | NoSQL document | Per-player profiles, sharded; DynamoDB for analytics |
| Supercell (state) | MySQL → Aurora (~300 DBs) | Relational (many-shard) | Early MySQL choice; Aurora cut ops; Scylla/DynamoDB for events |
| Roblox (platform) | CockroachDB/Mongo/Influx on Nomad | Mixed NoSQL/NewSQL | Self-hosted bare metal; risk shifted to Consul |
| RuneScape / OSRS | AWS Core save store + VM worlds | Relational-ish + shard VMs | Global account profile, regional edge worlds |
| Minecraft (world) | Anvil files / LevelDB (Bedrock) | Flat file / embedded KV | Local single-world; Realms adds Azure MySQL |
| Habbo / Club Penguin | replicated RDBMS + Java servers | Relational | Shared-world MMO persistence; optimistic locking |
| Valheim / Terraria / Factorio / Stardew | local serialized files | Flat file | Single-player / small co-op; no server |

---

## 3. Postgres-as-game-backing-store: failure modes and how tier separation avoids each

Each row is a real Postgres failure mode, and each is *avoided* by keeping the wrong workload out of
Postgres in the first place (Tiers 1/2/4/5).

| Failure mode | What happens | Docs | How tier separation / config avoids it |
|---|---|---|---|
| **Process-per-connection** | Each conn = one OS backend; shared memory sized off `max_connections` (default ~100, set at start) | [runtime-config-connection](https://www.postgresql.org/docs/current/runtime-config-connection.html) | Put a pooler in front — [PgBouncer](https://www.pgbouncer.org/) transaction mode multiplexes many clients onto few backends |
| **MVCC dead-tuple bloat** | High-churn `UPDATE`/`DELETE` leaves old row versions; VACUUM reclaims but doesn't return space to OS | [routine-vacuuming](https://www.postgresql.org/docs/current/routine-vacuuming.html) | Don't put per-tick sim state (Tier 1) or hot counters (Tier 2/Redis) in Postgres; tune autovacuum for the churny tables you keep |
| **WAL / fsync write amplification** | Every change logged before data files; `full_page_writes` logs whole pages after each checkpoint; short checkpoints = more I/O | [wal-configuration](https://www.postgresql.org/docs/current/wal-configuration.html) | Keep write rate low by not routing firehose telemetry (Tier 4) through Postgres |
| **Long-txn / migration locks** | `ALTER TABLE` takes ACCESS EXCLUSIVE, "conflicts with locks of all modes," blocks even `SELECT`; queues behind a long read | [explicit-locking](https://www.postgresql.org/docs/current/explicit-locking.html), [GoCardless](https://gocardless.com/blog/zero-downtime-postgres-migrations-the-hard-parts) | Set `lock_timeout` on migrations; follow [strong_migrations](https://github.com/ankane/strong_migrations) rules (CREATE INDEX CONCURRENTLY, no volatile-default add on huge tables) |
| **TOAST for big values** | Rows > ~2 KB compress/move out-of-line; fat rows get silently de-toasted on read | [storage-toast](https://www.postgresql.org/docs/current/storage-toast.html) | Blobs go to object storage (Tier 5), not `bytea` columns |
| **jsonb overuse** | GIN index bloat (`jsonb_ops` indexes every key); any update takes a **whole-row** lock | [datatype-json](https://www.postgresql.org/docs/current/datatype-json.html) | Model economy/inventory as real columns; reserve jsonb for genuinely schemaless blobs |

Scaling levers when you *do* stay on Postgres:

- **PgBouncer** in transaction pooling mode ([config](https://www.pgbouncer.org/config.html)) — the
  first fix for "too many connections."
- **Read replicas** via streaming replication — a hot standby serves read-only queries, at the cost of
  a small replication lag and possible loss of un-shipped txns on primary crash
  ([warm-standby](https://www.postgresql.org/docs/current/warm-standby.html)).
- **Partitioning append-only event tables** by time range — dropping/detaching an old partition is far
  faster than a bulk `DELETE` and "entirely avoid[s] the VACUUM overhead"
  ([ddl-partitioning](https://www.postgresql.org/docs/current/ddl-partitioning.html)).

> **→ Applies to this project.** On one VPS: (1) put **PgBouncer** in front from day one — it's cheap
> insurance against connection storms. (2) Keep sim state in `ggrs`, hot ephemeral state in-process or
> (later) Redis, telemetry in your `/ev` firehose, blobs behind nginx. If you follow that, the only
> churny thing Postgres sees is the economy ledger, which is append-heavy but not update-heavy (see
> §4) — exactly the low-bloat shape Postgres likes. (3) Use `lock_timeout` on every migration.

---

## 4. Economy / inventory correctness — the tier you cannot get wrong

Duplicated items or currency are usually unrecoverable (you can't un-print money once it's traded), so
this tier is defended with four complementary techniques.

**1. Idempotency keys** — the canonical pattern is Stripe's: save the result of the first request for a
given key; "subsequent requests with the same key return the same result," sent via an
`Idempotency-Key` header (a V4 UUID)
([Stripe idempotent requests](https://docs.stripe.com/api/idempotent_requests),
[Postgres implementation walkthrough](https://brandur.org/idempotency-keys)). Generate one key per
logical purchase/grant (per "buy" click) and persist it with the transaction row; a client retry or
network timeout then commits the grant exactly once.

**2. `SELECT … FOR UPDATE` (pessimistic locking)** — locks the retrieved rows so they can't be
"locked, modified or deleted by other transactions until the current transaction ends"
([SELECT](https://www.postgresql.org/docs/current/sql-select.html)). Inside a txn,
`SELECT balance FROM wallets WHERE user_id=$1 FOR UPDATE` serializes concurrent spends and eliminates
the read-then-write overdraft race.

**3. Ledger-not-balance / event-sourced double-entry** — the strongest structural defense. Martin
Fowler's [Event Sourcing](https://martinfowler.com/eaaDev/EventSourcing.html) calls accounting a
"particularly strong example"; fintech ledger engineering builds "on an immutable, append-only log"
where "money cannot be moved without specifying the source and destination," corrections are
"compensating entries," and double-entry validates that credits equal debits
([Modern Treasury](https://www.moderntreasury.com/journal/enforcing-immutability-in-your-double-entry-ledger)).
Why this beats a mutable `balance` column: a balance column is a destructive write with no history — a
lost update or double-apply silently and unrecoverably corrupts it. An append-only ledger records each
grant/spend as an immutable row, derives balance by summing (optionally snapshotting), and makes
currency-from-nothing bugs *assertable* — the books must balance, so a dupe produces a detectable
imbalance.

**4. Optimistic concurrency (version columns)** — validate "that the changes about to be committed…
don't conflict with the changes of another session" using a per-row version count (not a timestamp —
"system clocks are simply too unreliable")
([Optimistic Offline Lock](https://martinfowler.com/eaaCatalog/optimisticOfflineLock.html)). Read
`version=7`, then `UPDATE … SET balance=$new, version=8 WHERE id=$1 AND version=7`; if
`rows_affected=0`, someone else won — reject and retry. Scales better than pessimistic locking under
low contention, at the cost of retry logic.

> **→ Applies to this project.** Concretely for a single-Postgres economy:
> 1. Model currency and items as an **append-only ledger table**, not a `balance` column. Derive
>    balances by sum; add a periodic snapshot row if the sum gets slow.
> 2. Every grant/spend carries a client-generated **UUID idempotency key** with a `UNIQUE` constraint —
>    the DB itself rejects the duplicate on retry.
> 3. Wrap match-result payout in **one transaction**: this is the single write `ggrs` hands to
>    Postgres at match end. If you must mutate a summary balance, guard it with `FOR UPDATE` or a
>    `version` check.
> 4. This shape is also *bloat-friendly* (§3): inserts, not high-churn updates.

---

## 5. When to add each tier — honest triggers for a single-VPS cozy game

Default posture: **Postgres + `ggrs` + nginx on one box is a complete, correct architecture.** Add
tiers only when a measured trigger fires.

| Add… | Trigger (measured, not speculative) | Cheapest first step |
|---|---|---|
| **PgBouncer** | You see `FATAL: too many connections`, or connection count tracks request rate | Add it now — it's low-cost insurance, not really a "later" |
| **Redis** | Postgres CPU eaten by tiny hot reads/writes (presence, session, rate-limit counters), or you want a leaderboard that isn't an `ORDER BY` scan | One Redis on the same VPS; ZSET leaderboard + matchmaking |
| **Read replica** | Read-heavy dashboards/analytics queries steal capacity from gameplay writes; primary CPU is read-bound | One streaming standby, route read-only queries to it (accept lag) |
| **Warehouse (ClickHouse/DuckDB)** | "grep the event log" no longer answers product questions; you need ad-hoc aggregation over millions of events | Local ClickHouse or DuckDB-over-Parquet on the same box before any cloud warehouse |
| **CDN in front of nginx** | Asset download volume or geographic spread hurts; origin bandwidth saturates | Cloudflare/CloudFront pointed at the existing nginx origin |
| **Cassandra / ScyllaDB** | You have a genuinely multi-node write-throughput or dataset-size problem that one Postgres box + partitioning + replicas cannot hold — i.e. Discord/Supercell scale | You are almost certainly not here; revisit at millions of DAU |
| **Sharding (many DBs)** | A single primary can't hold the write rate even after PgBouncer, partitioning, and vertical scaling | Partition by player/region only when forced (WoW/Supercell shape) |

> **→ Applies to this project (bottom line).** For a cozy open-air hangout at hundreds-to-low-thousands
> of players: keep `ggrs` in RAM, keep **one** Postgres as system-of-record with a correct ledger,
> serve blobs from nginx, and keep the `/ev` firehose as append-only files. Add PgBouncer immediately.
> Add Redis when hot small ops or a real leaderboard justify it. Everything past that (replica,
> warehouse, CDN, and certainly Cassandra/sharding) is a response to a metric you have not hit yet.
> Do not pre-build it.

---

## Sources

**Tier 1 — realtime / rollback**
- [Gaffer On Games — Networked Physics](https://gafferongames.com/categories/networked-physics/)
- [Deterministic Lockstep](https://gafferongames.com/post/deterministic_lockstep/) ·
  [Snapshot Interpolation](https://gafferongames.com/post/snapshot_interpolation/) ·
  [State Synchronization](https://gafferongames.com/post/state_synchronization/)
- [GGPO](https://www.ggpo.net/) · [GGPO source](https://github.com/pond3r/ggpo) ·
  [ggrs (Rust)](https://github.com/gschup/ggrs)

**Tier 2 — Redis**
- [Redis Sorted Sets](https://redis.io/docs/latest/develop/data-types/sorted-sets/) ·
  [Leaderboard tutorial](https://redis.io/tutorials/howtos/leaderboard/) ·
  [Matchmaking tutorial](https://redis.io/tutorials/matchmaking-and-game-session-state-with-redis/) ·
  [Persistence](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/)

**Tier 3 — relational**
- [Postgres transactions tutorial](https://www.postgresql.org/docs/current/tutorial-transactions.html) ·
  [Amazon Aurora overview](https://docs.aws.amazon.com/AmazonRDS/latest/AuroraUserGuide/CHAP_AuroraOverview.html)

**Tier 4 — analytics**
- [ClickHouse observability](https://clickhouse.com/docs/use-cases/observability/clickstack/overview) ·
  [Google mobile gaming analytics](https://cloud.google.com/blog/products/gcp/build-a-mobile-gaming-analytics-platform) ·
  [Firebase BigQuery export](https://firebase.google.com/docs/projects/bigquery-export) ·
  [Snowflake key concepts](https://docs.snowflake.com/en/user-guide/intro-key-concepts) ·
  [Apache Parquet](https://parquet.apache.org/docs/) · [Kafka](https://kafka.apache.org/intro/) ·
  [Kinesis](https://docs.aws.amazon.com/streams/latest/dev/introduction.html)

**Tier 5 — object storage / CDN**
- [S3 durability](https://docs.aws.amazon.com/AmazonS3/latest/userguide/DataDurability.html) ·
  [GCS durability](https://cloud.google.com/storage/docs/availability-durability) ·
  [CloudFront](https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/HowCloudFrontWorks.html) ·
  [Cloud CDN](https://cloud.google.com/cdn/docs/overview)

**Real architectures**
- EVE: [High Scalability](https://highscalability.com/eve-online-architecture/) ·
  [Server tick](https://wiki.eveuniversity.org/Server_tick) ·
  [TiDi](https://www.eveonline.com/news/view/introducing-time-dilation-tidi) ·
  [DB hardware history](https://www.eveonline.com/news/view/a-history-of-eve-database-server-hardware)
- Discord: [How Discord Stores Trillions of Messages](https://discord.com/blog/how-discord-stores-trillions-of-messages) ·
  [InfoQ](https://www.infoq.com/news/2023/06/discord-cassandra-scylladb/)
- Minecraft: [Region file format](https://minecraft.wiki/w/Region_file_format) ·
  [Bedrock level format](https://minecraft.wiki/w/Bedrock_Edition_level_format) ·
  [Realms AWS→Azure](https://developer.microsoft.com/en-us/games/articles/2020/10/migrating-minecraft-realms-from-aws-to-azure/)
- WoW: [HowStuffWorks](https://electronics.howstuffworks.com/world-of-warcraft.htm) ·
  [GDC — Network Serialization and Routing](https://gdcvault.com/play/1017733/Network-Serialization-and-Routing-in)
- Supercell: [AWS Innovators](https://aws.amazon.com/solutions/case-studies/innovators/supercell/) ·
  [Aurora case study](https://aws.amazon.com/solutions/case-studies/supercell-aurora-case-study/) ·
  [ScyllaDB persisted events](https://www.scylladb.com/2025/01/14/how-supercell-handles-real-time-persisted-events-with-scylladb/)
- Epic/Fortnite: [AWS all-in](https://aws.amazon.com/blogs/gametech/epic-fortnite-all-in-on-aws-cloud/) ·
  [Postmortem (relay)](https://read.bytesizeddesign.com/p/how-a-34-million-concurrent-users) ·
  [MongoDB customer](https://www.mongodb.com/blog/post/ten-years-mongodb-customers-aws-reinvent)
- Roblox: [Return to service postmortem](https://blog.roblox.com/2022/01/roblox-return-to-service-10-28-10-31-2021/) ·
  [Portworx interview](https://portworx.com/blog/architects-corner-roblox-runs-platform-70-million-gamers-hashicorp-nomad/) ·
  [DataStoreService](https://create.roblox.com/docs/cloud-services/data-stores)
- RuneScape: [OSRS — Road to Greater Capacity](https://oldschool.runescape.wiki/w/Update:More_Worlds,_More_Power:_The_Road_To_Greater_Capacity) ·
  [runescape.wiki/Server](https://runescape.wiki/w/Server)
- Habbo/CP: [Habbo architecture PDF](https://h4bbo.net/archive/Habbo%20Monster%20Archive%20!%20-%20by%20Wendigo/Others/habboarchitecture.pdf) ·
  [CP Wiki — Server](https://clubpenguin.fandom.com/wiki/Server)
- Indie: [Valheim](https://xgamingserver.com/docs/valheim/world-save-files) ·
  [Terraria](https://seancode.com/terrafirma/world.html) ·
  [Factorio blueprint format](https://wiki.factorio.com/Blueprint_string_format) ·
  [Stardew](https://steamcommunity.com/app/413150/discussions/0/405692758715239357)

**Postgres failure modes**
- [Connections](https://www.postgresql.org/docs/current/runtime-config-connection.html) ·
  [PgBouncer](https://www.pgbouncer.org/) ·
  [MVCC / vacuuming](https://www.postgresql.org/docs/current/routine-vacuuming.html) ·
  [WAL config](https://www.postgresql.org/docs/current/wal-configuration.html) ·
  [Explicit locking](https://www.postgresql.org/docs/current/explicit-locking.html) ·
  [GoCardless migrations](https://gocardless.com/blog/zero-downtime-postgres-migrations-the-hard-parts) ·
  [strong_migrations](https://github.com/ankane/strong_migrations) ·
  [TOAST](https://www.postgresql.org/docs/current/storage-toast.html) ·
  [jsonb](https://www.postgresql.org/docs/current/datatype-json.html) ·
  [Partitioning](https://www.postgresql.org/docs/current/ddl-partitioning.html) ·
  [Warm standby / replicas](https://www.postgresql.org/docs/current/warm-standby.html)

**Economy correctness**
- [Stripe idempotency](https://docs.stripe.com/api/idempotent_requests) ·
  [Idempotency keys in Postgres](https://brandur.org/idempotency-keys) ·
  [SELECT FOR UPDATE](https://www.postgresql.org/docs/current/sql-select.html) ·
  [Fowler — Event Sourcing](https://martinfowler.com/eaaDev/EventSourcing.html) ·
  [Modern Treasury — immutable ledger](https://www.moderntreasury.com/journal/enforcing-immutability-in-your-double-entry-ledger) ·
  [Fowler — Optimistic Offline Lock](https://martinfowler.com/eaaCatalog/optimisticOfflineLock.html)

*Verified July 2026. `postgresql.org/docs/current/` links track the latest release — pin to
`/docs/17/` for a stable reference. Flagged items (EVE instance topology, WoW/Habbo exact RDBMS,
Fortnite MCP numbers via relay, RuneScape `.dat` format) are secondary-sourced.*
