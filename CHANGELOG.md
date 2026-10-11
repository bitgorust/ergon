# Changelog

## [Unreleased]

### Added
- Document what the loop thread of each lab app allocates and which system
  calls it makes, in the runtime README's "Allocations and system calls on
  the loop thread". Steady-state tests of md, the engine and exch-sim count
  every allocation on the loop over 5.5 s of traffic and fail on one; under
  `strace` on Linux the same windows show only the 10 ms external check
  (`ppoll`) and the 5 s allocator sample (`getrusage`), none per message.
- Print a pod's log lines with `ergon-runtime-log`, a new crate under the
  server's rules. `Printer::spawn` subscribes untethered to the log stream
  and writes each row of a source whose `Source.pod` is its own as one
  logfmt line (`ts=… level=… target=… message=…`, quoted and escaped so a
  line stays one line). `PrinterGuard` stops it: it prints until the pod's
  stream ends and 100 ms pass with nothing to print, or 8 s pass, then
  joins. A slow output costs only the printer's lines: they wait in a 1 MiB
  buffer, and when it is full they are dropped and a `WARN` line says how
  many. A printer that stops polling holds every application's log
  publication on the node, so each drops its lines at the publisher
  (`log_dropped{reason="back_pressure"}`) and the `log` table misses them,
  for up to about 4 s (0.8 s measured) until the driver drops the printer.
  A hung printer thread rejoins after 60 s; a sidecar stopped with SIGSTOP
  has its client timed out after 10 s and exits with an error when
  continued. The log publication now sets those untethered timeouts; Aeron's
  defaults cost about 10 s of lines in every 20 s.
- Add `log: { output: thread }`: lines go to the log stream as with `aeron`,
  and the application prints them itself. The lab's `lab::runtime` starts the
  printer for it and now returns it beside the runtime: keep it until the
  loop has finished. `thread` was refused before.
- Give the lab's engine and exch-sim pods a `logs` native sidecar (an init
  container with `restartPolicy: Always`, 128 MiB) that prints the pod's
  lines: `kubectl logs <pod> -c logs`. Their cold files now set
  `log: { output: aeron }`. `just logs` follows every container
  (`--all-containers`, up to 64 streams), and `just test` checks both renders
  for the sidecar (`scripts/test-logs-sidecar.sh`).
- Read every lab container's output in Grafana's Explore: the lab now runs
  Loki (one process, 72 h on its pod's disk) beside Grafana, an Alloy on each
  node that tails `/var/log/pods` and labels lines with `app`, `pod`,
  `container` and `node`, and a provisioned **Loki** data source. Both have
  CPU and memory limits. `just test` renders them and lints Grafana's
  provisioning and dashboards, and runs Loki's and Alloy's own validators on
  their configs (`scripts/test-loki.sh`); `just verify` checks that each app
  pod and `logs` sidecar has lines there. A lab deployed before this
  restarts Grafana once to load the data source.
- Compare a raw Aeron loop with the ergon runtime in `aeron-bench`:
  `--loop raw|runtime` on `ping` and `pong`, and `ping --gate`, which runs
  both loops in turn (`--rounds`, default 4, so each runs first equally
  often), each against a `pong` of its own, and exits 1 when the
  runtime's median p50 or p99 round trip is above the raw loop's
  (`--seed-regression` proves it can fail). It takes `--size` above 32
  only, so pings and answers differ in length. `scripts/aeron-matrix.sh` runs
  each cell in both loops, except the invoker-driver and archive cells, which
  the runtime loop cannot run, and `aeron-report.py` keeps the loops apart.
- Configure the lab's ingester as the other applications are: identity from
  the environment, and a cold file (`ERGON_CONFIG`, else
  `/lab/cold/app.yaml`) with `bus.aeron_dir`, `config_dir` and an `app:`
  section (`schemas`, `checkpoint`, `interval`, `clickhouse`, `stream_id`,
  `log_stream_id`). The ClickHouse password stays `CLICKHOUSE_PASSWORD`, from
  the `clickhouse` Secret, and `CLICKHOUSE_DATABASE` lays over `database`.
  SIGTERM or SIGINT now ends its wait between ticks at once, its
  last inserts checkpointed; one during start-up exits at once, and a second
  SIGINT exits with 130. It reports its own `session` rows (status `Running`
  and how many recordings it follows) through its archive, as an application
  does, and logs at the level `log.yaml` gives it.
- Set log levels in the global config's `log.yaml` (`logs::Levels`): `level`
  for every application and `apps` by name. Only the ingester reads it, at
  start and within a tick of a change; a loop application follows
  `log.output` (`RUST_LOG` for `stderr`, `info` for `aeron`). The lab's
  `lab-config` carries it.
- Wait for SIGINT or SIGTERM outside a loop: `Signals::wait(timeout)` arms
  the signals and consumes one with the loop's second-Ctrl-C latch, and
  `Signals::arm` is public. `source::stamp` writes a `Source` message's
  `sent` time for a process that sends its own.
- Write the ingester's open histogram windows when it stops: call
  `Ingester::stop` before dropping it, as the lab's ingester now does on
  SIGTERM, so a rollout no longer loses up to 5 s of each histogram series.
  `Ingester::is_pending` says when a batch `ClickHouse` refused is left for
  the restart, which the lab's ingester logs at its stop.
- Send an ingester's own session: set `ergon_runtime_server::Settings.source`,
  and its `Source` message, with a session, goes on the persist stream every
  5 s, so its own `session` rows show it live.
- Say what an application is doing on its feeds: `Ctx::set_session(&value)`
  puts `value`, as JSON, in the `Source` message every output sends, at once
  and again every 5 s, beside a runtime-owned `source::Status` (`Starting`,
  `Running`, `Draining`, `Stopping`, each sent at once) and when it was sent;
  one an output did not take (back pressured, or no subscriber) goes ahead
  of its next frame.
  Subscribers read it in-band with `source::Session::decode`, which does not
  allocate. A session whose message would not fit the smallest output's frame
  is refused, as is an output opened later whose frames are too small for it.
  Persist's, the bridge's and a `Publication`'s copies stay identity only.
  This is version 3 of the event schema: older readers take version 3
  `Source` messages, and this one takes theirs.
- Keep each recorded feed's sessions in the ingester's `session` table (`ts`,
  `status`, `session`, `source`, and who sent it), always on like
  `metrics`; `tables.yaml` may give its database. `session` is now a
  reserved table name. A `Source` message whose
  session changed is not a new origin: the saved dictionary grows only with
  a new identity, and holds version 2 and 3 messages alike. The lab's Grafana
  gains a `Sessions` dashboard (live sessions, config revisions) and an alert
  on `config_rejected`.
- Replay a run's config without its files: with the journal on, each
  watched text is recorded as a `config` row (`outcome: text`) ahead of the
  `Config` row naming it, under the journal's policy, and a text longer than
  one row is refused. `journal::clickhouse::configs` reads them into the new
  `Journal::configs`; a replay's `Ctx::watch` returns the startup text from
  the journal and each revision applies the journal's text, reading no
  file. The engine backtest loads them.
- Watch an application's own config files live: `Ctx::watch(ConfigFile {
  name, version })` reads `<name>.v<N>.yaml` in `config_dir` during setup.
  Each text is checked by `Agent::check_config` (validate and resolve, with no
  side effect) and adopted by `Agent::apply_config`; on `Err` the runtime
  discards the feeds, outputs, timers, ids and directory the apply set up.
  Files are applied before `Agent::start`, and a refusal stops the start;
  while running, a refused text is logged, counted in `config_rejected{file}`
  and written as a `config` row, and the last applied one stays. A `v{N+1}`
  file beside `v{N}` is refused the same way, once, its `config` row under
  `v{N}`'s file (the one `config_rejected` counts) and the reason naming
  `v{N+1}`. The input journal holds a
  `Config` row (kind 7: the hashes of the file's name and of the text)
  before each apply and another after one rolled back.
  `Ctx::feed` resolves a feed's address without opening it.
- Publish the loop's `log` lines as rows of the `log` table on the log stream
  with `log.output: aeron` (`logs::Logger`, `logs::LoopLog`): no allocation
  or system call per line once warm, never blocking. A line Aeron cannot take
  is dropped and counted in `log_dropped{reason}`, and a `WARN` line says how
  many once the stream takes lines again; a message over 16 KiB is cut. Other
  threads' lines, and the loop's after shutdown, go to standard error.
  `stderr` stays the default. `Invoker::finish` closes
  the stream with persist's; a loop the runtime does not own calls
  `logs::close`. The runtime's `Settings.log_stream_id` names another stream.
- Record the log stream in the ingester's archive beside the persist stream:
  stream 1004 (`persist::LOG_STREAM_ID`, documented with the other reserved
  ids), read as the persist stream is, its rows going to the event table `log`
  (`persist::LOG_TABLE`). `Settings.log_stream_id` names another stream.
- Add idle strategies with parameters:
  - `runtime.idle: sleep:<period>`;
  - `runtime.idle: backoff[:<spins>,<yields>,<min park>,<max park>]`, Agrona's
    `BackoffIdleStrategy`, which spins, yields, then parks for doubling
    periods and starts over on any work;
  - `runtime.timer_slack: <period>`, which sets the loop thread's Linux timer
    slack so short parks wake on time.

  The lab's engines and dummy exchanges now back off instead of sleeping
  1 ms.
- Configure a runtime process from one cold YAML file read at start-up
  (`config::Cold<A>`): `ERGON_CONFIG`, else `/lab/cold/app.yaml`, else the
  defaults, with `runtime:`, `persist:`, `bus:`, `log:`, `config_dir` and the
  application's own `app:` section. `PROFILE` merges one of the file's
  `profiles` over it. An unknown profile, key or value stops the start, in
  any of the file's profiles whether chosen or not.
- Read a process's identity in one place, `config::Identity::from_env`:
  `NODE_NAME`, `POD_NAME`, `POD_IP`, `HOST_IP`, `REGION`, `PERSIST_APP`,
  `SERVICE`, `EXCHANGE`, `ERGON_CONFIG` and `PROFILE`. It is the runtime's only
  read of the environment, and a test fails on any other in its source;
  `env_logger` still reads `RUST_LOG` for `log.output: stderr`.
- Gate releases on `just bench-runtime`. Both `just release` and
  `just release-verified` run it, then rerun each gated `ergon-runtime` bench
  with `-- --seed-regression`, which must make every gate fail. The benches
  with several gates (`timer`, `hotpath`) are also rerun with
  `--seed-regression=NAME`, and that one failing gate must fail the run.
  The `dispatch` bench fails within seconds, rather than hangs, when a feed
  frame is dropped or a message is lost, which `--seed-loss` proves.
- Add `just k9s`, which opens k9s on the lab's cluster, and have `just urls`
  print the ClickHouse password with each UI's address.
- Generate an opt-in cross-schema AnySchemaMessage dispatcher with typed errors
  and borrowed frames for persistence, including unknown schemas and templates.
- Report the cluster client's ingress limits, so a client can refuse to start
  when its largest message would not fit: `AeronCluster::ingress_max_message_length()`
  (Java `ingressPublication().maxMessageLength()`), `ingress_max_payload_length()`
  and `AeronCluster::SESSION_HEADER_LENGTH`. `offer` takes payloads up to the
  first less the header, and `try_claim` up to the smaller of the two less the
  header.
- Report the egress image a cluster session is bound to:
  `AeronCluster::egress_image_correlation_id()` and `egress_image_session_id()`.
  As in Java, the session binds to the image that delivered its
  `SessionEvent(OK)` or latest `NewLeaderEvent`, so a replaced image shows as a
  new id.

### Changed
- Breaking (generated API): drop `try_` from the per-field and closure
  methods.
  - A domain-typed field's getter and setter take the plain field name
    (`price()?`, `price(v)?`); the raw accessor stays `price_wire`. A field
    named after a reserved method takes the same `_field` rename its raw
    accessor would.
  - The decoder's boolean getter is `{field}_bool()` again, reversing
    0.1.24's `try_{field}_bool()`; it shares its name with the encoder
    setter, and only the return type varies with shape.
  - The `Ordered` lane's `try_fixed`, `try_<group>`, `try_<data>` and
    `try_<data>_as_str` twins are removed; their plain forms return
    `DecodeError`; map a custom error into it inside the closure.
  - The raw stages' `try_<data>(f)` and `try_<data>_as_message(f)`
    callbacks are removed; use `into_<data>()` / `into_<data>_as_message()`,
    or `.ordered()`.
- Check ergo-sbe's wire bytes against sbe-tool 1.40.2, up from a pre-1.40.0
  snapshot: the reference codecs and the benchmark comparators are
  regenerated from it, and the parity suite adds its `issue1116`,
  `issue1118` and `floating-point-infinity-schema` schemas plus five more of
  its test schemas (an explicit message `blockLength`, lower-case set choices
  and enum values, `sinceVersion` messages and constant group fields).
- ergo-sbe rejects a float or double `nullValue`, `minValue`, `maxValue` or
  constant spelled `inf`, `infinity` or `nan` in any case other than
  `Infinity` and `NaN` (with an optional sign), as sbe-tool does. A malformed
  float constant now fails at parse time, not in the generated module.
- ergo-sbe rejects `presence="constant"` on a set or composite field, and a
  constant enum field given by `constantValue` rather than `valueRef`. A
  constant takes no block space, so these read the wire past the block (or
  failed to generate); sbe-tool rejects the enum form too.
- `aeron-bench ping`'s JSON line adds `mean_ns`, to read beside `p50_ns`,
  which the gate uses. `ping --gate` gives each run a driver of its own
  when it starts one, as a killed `pong`'s client slowed the runs after
  it. The raw loop now does
  the runtime's work for each frame: it reads the runtime's clock for each
  message it takes, claims and writes each frame in place rather than
  copying it with `offer`, and sends the runtime's `Source` frames where
  the runtime would.
- `aeron-bench`'s raw loop runs its Aeron client's conductor in the loop
  (invoker mode) and publishes exclusively, as the runtime does, and both
  loops take the runtime's idle strategies. `--idle backoff` is now Agrona's
  10 spins, 5 yields and 1 µs–1 ms parks, not 100, 100 and 2 µs–1 ms, so
  backoff results do not compare with older runs, which `aeron-report.py`
  labels `backoff(old)` and leaves out of its idle chart. Frames carry an 8-byte
  header, so `--size` is at least 24 bytes, and the JSON line's `idle` is the
  spec as given (`spin`, not `Spin`). `pong` answers each ping with its
  first 24 bytes rather than all of it: with pings and answers of one
  length, the offset the streams' first frames left between them stayed
  fixed and alone moved p99 about threefold on an M4, so results do not
  compare with older runs either.
- Breaking: have the lab engine watch `streams.yaml` and its own
  `engine.v1.yaml` through `Ctx::watch` (`Engine::watched`), so the input
  journal holds the registry it started with and each revision: a feed
  handler added mid-run is subscribed in service order, and a replay
  subscribes to it at the same point under the same feed ids, with the files
  gone. Set each venue's `gap_rule` in `engine.v1.yaml`, which replaces the
  `GAP_RULE_<VENUE>` variables; the engine refuses to start without the file.
  `Engine::new` takes a fixed registry and `EngineConfig`; `Engine::watching`,
  its once-a-second registry timer and `GapRule::parse` are gone.
  `backtest --source journal` reads both from the journal, needs no
  `streams.yaml` on disk and refuses `--streams`, so it no longer replays a
  run journaled before this change. A file, archive or ClickHouse backtest
  takes the engine's rules from `--engine PATH`, by default `engine.v1.yaml`
  beside the registry file (the fixture's for `file`); journal modes refuse
  it.
- Breaking: `journal::Journal` has a `configs` field, the texts its `Config`
  rows name; build one with `..Journal::default()`.
- Breaking: every `Agent` declares `type Candidate`, what its
  `check_config` makes of a config file; `type Candidate = ();` for one that
  watches none.
- Breaking: have the ingester write every row it receives, whatever
  `tables.yaml`'s `enabled`, `apps` or `until` say: a switch only stops an
  application recording a table from then on. Every message's table,
  `frame`, `input`, the journal's `config` and the ingester's `coverage` are
  static, kept forever and never dropped; `kind: dynamic` or `ttl:` on one is
  refused, as is a switch other than `true` on a message's table, `frame`,
  `input` or `coverage`. A static message table adds its message's new
  fields only with `add_columns: true`. A dynamic table's rows expire a day
  after they are written (the ingester's `DYNAMIC_TABLE_TTL`, or the table's
  own `ttl:`), and an empty dynamic table is dropped, made again when rows
  arrive.
- Breaking: name each node's archive in what the ingester writes, since
  recording ids restart at 0 on every node. `Settings::new` takes the
  archive instance (`Settings::archive`, at most 255 letters, digits, `.`,
  `-` and `_`), and the sample ingester requires `NODE_NAME`. `Batch::new`
  and every insert's dedup token name it, so two nodes' batches of the same
  positions both land. `frame` gains `archive` and `key` columns (the
  message's `semanticType="key"` fields as little-endian wire bytes, empty
  when unkeyed) and a `key_bloom` skip index. An existing `frame` table, now
  static, gains neither column by itself: run the `ALTER` the ingester logs,
  or drop the table, and only a table made afresh has `key_bloom`.
- Breaking: each commit publishes the saved checkpoint as rows of a new
  static `coverage` table, and the ingester purges the archive only behind
  coverage ClickHouse acknowledged. An ingester that finds another
  archive's coverage under its instance name (its archive or checkpoint was
  reset while ClickHouse kept the rows) refuses to start, before it reads or
  inserts anything, with the new `Error::Instance`.
- Read the loop's and persist's settings from the cold file instead of
  `IDLE`, `TIMER_SLACK`, `CPU`, `MLOCK`, `JOURNAL` and the `PERSIST_*`
  variables, which the runtime no longer reads. Periods take `ns`, `us`, `ms`
  or `s`. `Settings::from_env` returns a `Result`; `Settings::from_cold`,
  `Invoker::from_cold` and `Runtime::from_cold` take a loaded identity and
  file; `App::start` replaces `App::start_with`; `Idle::from_env`,
  `source::host_name` and `app::Error::Idle` are gone.
- Give each lab application its own cold ConfigMap, `<app>-cold`
  (`md.cold.yaml`, `engine.cold.yaml`, `exch-sim.cold.yaml`,
  `ingester.cold.yaml` under `deploy/`), mounted at `/lab/cold/app.yaml` and
  named by `ERGON_CONFIG`. Its name carries a hash of the file, so an edit
  restarts that application and nothing else; `scripts/test-cold-config.sh`
  (in `just test`) checks it. `lab::cold` and the ingester no longer read
  `PERSIST_CONFIG`, `PERSIST_SUBSCRIBER_TIMEOUT`, `IDLE`, `JOURNAL`,
  `PERSIST_SCHEMAS`, `PERSIST_CHECKPOINT`, `PERSIST_INTERVAL`,
  `CLICKHOUSE_URL` or `CLICKHOUSE_USER`, and the manifests no longer set
  them. md gets `REGION` from its overlay's `region` label; `just verify`
  still names a feed handler `md-<exchange>`, and
  `scripts/test-publisher-names.sh` (in `just test`) checks every publisher
  name it builds is a Service.
- Make `just spin` a kustomize overlay (`deploy/spin/`) applied server-side
  like `just deploy`, instead of `kubectl patch`, which conflicted with
  server-side apply's field ownership; `just deploy` undoes it. The engine
  and the dummy exchange spin through their cold files' `spin` profile
  (`PROFILE=spin`). The lab's workloads move to `deploy/lab/` and `lab-config`
  to `config/kustomization.yaml`; `kustomization.yaml` lists both.
- Keep the lab's ClickHouse password in the Kubernetes Secret `clickhouse`,
  which `just deploy` makes from `CLICKHOUSE_PASSWORD` in `.env`. ClickHouse,
  the ingesters, Grafana and Jupyter read it from there, and `just verify`
  reads it with `kubectl`. No manifest holds a password any more.
- Scalar body encode benchmarks write successive preallocated destinations with
  symmetric input opacity, instead of measuring repeated black-box stack spills.
- **`optional_enum_nullify` is gated at 1.00 in both profiles.** Both arms
  keep four independent totals of the same three decoded fields, so the row
  measures decode throughput. The 1.01 allowance was for a single running
  total, which measured loop latency.
- Run each `ergon-runtime` client on one thread. `Bus` has one owner and runs
  Aeron's conductor from the loop, never on a thread of its own, so
  `Settings.aeron_invoker` and `AERON_INVOKER` are gone. `Persist`,
  `Metrics` and its handles, and `Tracer` are not `Send`.
- Record persist's rows only through the loop's `Persist`, on an exclusive
  publication of its own. `Settings.exclusive`, `Persist::install`,
  `Persist::bus` and the free `persist::record*`, `metrics` and `tracer`
  functions are gone.
- Record `tracing` events that name a `table`, and spans, from any thread
  through `Persist::layer`. The bridge publishes on a concurrent IPC
  publication of its own, and applies `enabled`, `apps` and `until` to
  those rows and to `otel_traces` for its app before publishing.
- Record metrics only through `Metrics` handles. `tracing` events with a
  `counter`, `gauge` or `histogram` field are no longer recorded, and
  `LocalHistogram` is merged into `Histogram`.
- Handle SIGINT and SIGTERM in the runtime (`rt::Signals`,
  `Config::signals`):
  - a startup guard exits with 128 + signo until the loop runs
    (`rt::StartupGuard` alone for an app whose framework owns its signals);
  - `Agent::on_signal` returns `OnSignal::Stop` or `OnSignal::Drain`;
  - one deadline bounds shutdown (`Config::stop_timeout`, 3 s), and a
    message's journal write stalled by back pressure gives way to a signal:
    the journal ends there, that message and later ones are not dispatched,
    and the loop stops after `on_signal` even on `OnSignal::Drain`, so the
    journal stays an exact prefix;
  - a second Ctrl-C always exits with 130; SIGTERM is never forced, and
    exits with 143 (128 + SIGTERM) once `Signals` is dropped. Install one
    `Signals` per process: after the drop both signals exit, so a later
    install never shuts down gracefully.

  `Invoker::finish` runs once, and closes the outputs and persist's
  publication. With `runtime.journal` on, the runtime records a `Signal` row
  per signal, and `finish` records a `Completed` row after `Stop`, once the
  agent stopped and the outputs closed.
- Replay a journal's `Signal` rows through `Agent::on_signal`, every row
  after a `ctx.stop()` as live dispatched it, and `Agent::stop` under the
  `Stop` row's context. A replay, and `journal::clickhouse::load`, now reject
  a journal that does not end `Stop`, `Completed`. Replay a crashed or
  signal-cut run with `SimConfig::journal_prefix` (loaded by
  `journal::clickhouse::load_prefix`): `Sim::run` returns
  `Outcome::Incomplete`, does not call `Agent::stop`, and drops and counts a
  timer batch the journal ends inside; `Outcome` is `#[must_use]`, so an
  ignored `Incomplete` warns. The sample's `backtest --source
  journal-prefix` replays such a run, including one recorded before
  `Completed` existed, and reports it incomplete.
- Re-read `tables.yaml` only when the config directory changes. The
  runtime's 10 ms check of the signal sockets also polls an inotify watch
  (a kqueue on macOS) of the directory, which sees the kubelet's `..data`
  swap, and applies a change with `Persist::reload` at the tail of a cycle
  that found no work. `Persist::poll` no longer `stat`s the file every second;
  it still applies the switches again then, so an `until` passes by itself.
  An application that drives `Persist` without the runtime calls
  `Persist::reload` itself. If the directory cannot be watched, or the
  directory itself is later removed or renamed, one error is logged and live
  reload is off. On macOS an in-place write of the file is not seen; replace
  the file instead.
- Resolve feed names through the application's `directory::Directory`
  (`rt::Config::directory`, `Ctx::set_directory`). Its one method,
  `feed(service, kind)`, returns a typed `directory::Feed`: a stream id and an
  `Endpoint` (`Ipc`, `Udp(HostPort)`, or `Mdc { publisher, reliable, archive }`),
  for subscribing and publishing alike. The runtime builds every channel from
  it at this node's address, so a directory holds no Aeron URIs and no node
  IP. `Bus::subscribe` and `Bus::subscribe_live` take a `Feed`.
  `Ctx::subscribe_at(&feed)` and `Ctx::publish_at(&feed)` open a feed the
  directory does not name, and replace `Ctx::subscribe_channel` and
  `Ctx::publish_channel`; a simulation names such a feed
  `<endpoint>/<stream id>` (`ipc/101`), the same for both. `rt::ArchiveConfig`
  takes `control: Endpoint` (`Ipc` for this node's archive, `Udp` for a remote
  one) and derives the response channel; its `response` field is gone, and
  the backtest's `ARCHIVE_CONTROL` takes `<host>:<port>` and
  `ARCHIVE_RESPONSE` is no longer read. The `streams.yaml`
  registry moved from `ergon-runtime` to the lab's `lab` crate, and the
  `ingester` binary from `ergon-runtime-server` to
  `samples/clickhouse/ingester`.
- Keep Aeron out of the runtime's public API, which a test now checks:
  - `Bus::publication` takes a `directory::Feed`, as `Ctx::publish_at`
    does; `Bus::subscription(channel, stream)` is gone (use
    `Bus::subscribe_live`);
  - `Settings.channel` is gone from both crates, and so are
    `persist::CHANNEL`, `logs::CHANNEL` and `bridge::BRIDGE_CHANNEL`. The
    ingester records the persist and log streams by the same strings as
    before;
  - `ergon_runtime_server::RecordedFeed` takes a `feed: Feed` instead of
    `stream_id` and a `spy` URI, and the ingester builds the spy at its
    `Settings.host_ip` (`HOST_IP`). `lab::Streams::spy` is gone;
  - `Error::Aeron` is `Error::Bus` in `ergon-runtime` and
    `ergon-runtime-server`, displayed as `bus: …`.
- Always set the media driver's directory on the client: `Settings.aeron_dir`
  and the cold `bus.aeron_dir` are a `String`, default `/aeron/driver`, and
  `AERON_DIR` is no longer read by the runtime or the ingester.
- Remove `ergon_runtime_server::Settings::from_env`: build `Settings::new`
  from the caller's config, as the ingester binary does from its cold file.
- Remove unused runtime API: `rt::Mode::Backtest` (`Mode::Replay` runs
  backtests), `Runtime::invoker` (use `Invoker::new`), `App::start`,
  `App::metrics` and `App.region`, `Ctx::is_sim` and `Ctx::sim_feeds`,
  `Bus::publish`, `clock::SimClock`, `TraceId::of`, `Journal::decode`,
  `frames::Record::end`, the `metrics::bucket*` functions and
  `Writer::wants`. `Mode`'s variants box their configurations.
- The runtime timer benchmark's gate and Criterion time the same function.
  A gate sample is the mean of 64 runs, and the heap baseline is always
  inlined. Ungated probes also report the simulation's step, with up to 1,024
  timers parked on one spoke, and the next-deadline search after the earliest
  timer is cancelled.
- Lay the runtime timer wheel out as one 32-byte record per timer, each spoke
  a linked list through the records with its earliest deadline at the head,
  so a schedule pops a free handle, writes one record and links it into its
  spoke, and the next-deadline search reads one record per occupied spoke,
  walking a spoke's list only once its earliest timer has left. The wheel now
  beats the heap baseline in every gated scenario in both LTO profiles; the
  per-spoke slab lost on ascending schedules. In the ungated simulation
  probes (the step, and the search after cancelling the earliest), the heap
  is still faster.
  `Settings::timers_per_spoke` sizes the slab as an average: a crowded tick
  no longer doubles every spoke, the slab doubles only when every record is
  in use, and a re-arm never needs room, so a repeating timer is never
  dropped. A default wheel takes 2.28 MB, up from 2.15 MB. The first release
  and the first full batch after a doubling do not allocate.
- The runtime claim benchmark writes each arm through its whole log before
  timing. A first touch of a sparse log page is then never timed, where
  before only one arm paid it.
- Stop blocking in the cluster client's polls. `AsyncClusterConnect::poll`
  adds the egress subscription, the ingress publication and a REDIRECT's
  publication with Aeron's async add and picks them up on later polls, and
  `poll_egress` handles a `NewLeaderEvent` the same way; each add used to wait
  up to 5 s. Only the first `poll`, which creates the Aeron client, can still
  wait, and only while the media driver is not running.
- Follow a new cluster leader as Java's client does. After a `NewLeaderEvent`
  the session stays `AwaitingNewLeader` until the new leader's ingress
  publication has connected, within `message_timeout`; it used to be
  `Connected` as soon as the publication was added. If that leader's egress
  image closes first, the session waits for another `NewLeaderEvent` within
  `new_leader_timeout`. The last `NewLeaderEvent` in one poll wins, and the
  `Disconnected` reason names what the session was waiting for.
- Detect cluster leader loss from the session's own egress image:
  `AeronCluster::is_egress_connected()` is false once that image closes, even
  while a replacement image is open. It used to count the subscription's
  images.
- Return `PayloadTooLarge` from `AeronCluster::try_claim` for a claim over the
  ingress publication's max payload length, instead of Aeron's publication
  error. `connect` and `finish` fail when the publication's limits cannot be
  read, rather than skip the size checks.

### Fixed
- Leave constant fields (declared on the field or on its type) out of a
  group entry's `blockLength`. ergo-sbe counted a constant that ends an
  entry, so each entry was wider than sbe-tool's; frames ergo-sbe encoded
  for such schemas change width. Message blocks were already right.
- Return a constant enum in a group entry from its `valueRef`, as message
  level does. The entry accessor read the wire past the entry's block; its
  entry setters and its `raw_` and `*_bool` accessors, which wrote or
  read there, are removed.
- Count a group entry's DTO `encoded_length()` from the stride the encoder
  writes. It ignored a group's declared `blockLength` padding and took the
  width of a nested group's entry when that was wider (the Car example's
  `performanceFigures` entries by 5 bytes each).
- Take an enum's null from its `encodingType` typedef's `nullValue` when the
  enum declares none, as sbe-tool does: `NullVal`, the `From` conversions and
  an optional enum field's `None` now write that value (254 for
  `nullValue="254"`), where they wrote the primitive default.
- Generate float and double constants written `Infinity`, `-Infinity`,
  `+Infinity` or `NaN`; they produced code that did not compile. A negative
  constant such as `-1.5` is no longer mistaken for an `Enum.value` reference.
- Decode an optional float or double whose `nullValue` is not NaN (such as
  `Infinity` or `-1.0`) as `None` when it holds that value, at message level,
  in group entries and in composite members. Only a NaN null still matches
  any NaN.
- Remove the per-message allocations found on the lab apps' loops. The
  engine's order books are sorted vectors that keep their capacity: a book
  snapshot allocated every node of the old maps. It reserves 256 levels a
  side and 256 open orders. It compares an instrument spec, which md repeats
  before every snapshot, before copying its asset name.
  `Persist::record_row` keeps up to 32 fields on the stack, where it
  collected them into a `Vec` for each row, such as md's one per Hyperliquid
  public trade.
- Publish metrics without allocating on the loop, the first summary
  included: registering a series now sizes every buffer a publish reuses.
  The first histogram summary grew three of them, and with `mimalloc` the
  5 s `alloc_sample_ns` histogram allocated on the loop shortly after start.
- Refuse a short or corrupt `Source`, `Shape`, `MetricDef` or `TraceDef`
  message with `None` from its `decode`, which now uses the checked
  `try_decode`. They used the trusted tier, which panics on a frame shorter
  than its header or fixed block, and the ingester hands them archive bytes
  after reading only the schema id.
- Decode an order in the lab's dummy exchange and simulated venue with the
  generated `AnyMessage` decode. Both checked the schema id by hand and then
  used the trusted decoder, so a 6- or 7-byte frame carrying that id panicked.
- Look up a journalled message from a recording by the recording's session
  as well as its id, and compare frames only at the positions the journal
  names. A persistent subscription now journals the recording's session for
  replayed messages too, not the replay image's. After a publisher moves
  between nodes, two archives can number a recording of the same feed alike,
  and an exact replay of such a journal failed, or took another session's
  frame. A journal recorded before this still replays: a replayed message
  the session does not find is looked up by its recording alone, as then.
- Cut each batch the ingester inserts by its records' places in the archive,
  not by place in its queue: each recording's part of the batch, laid end to
  end and cut every 1 MiB. After a crash the restart reads the batch again,
  and if a switch had changed meanwhile, or its recordings arrived in another
  order, the pieces moved: ClickHouse then dropped pieces holding rows that
  had never gone in, and let others in twice. A batch now reads at most 64 MiB
  of the archive, so its pieces stay inside ClickHouse's deduplication window.
  Let a batch an older ingester left pending (`checkpoint.pending`) go in
  before upgrading, since it is cut the new way.
- Insert each day of each piece of an ingester batch as one block of its
  own, named by the piece and the day, with the settings that keep it one
  block and deduplicated (`max_insert_block_size`, `insert_deduplicate`,
  `async_insert`) sent with it. ClickHouse splits an insert by partition and
  by a profile's lower `max_insert_block_size`, and does not promise to take
  all of its blocks or none, so exactly-once no longer rests on it. A pending
  batch file now starts with `v2`. One without it is finished cut by piece
  alone, as the ingester that wrote it did.
- Sync the ingester's pending batch and checkpoint files, and their
  directory, to disk before an insert or an archive purge relies on them. A
  host crash could leave either one empty or as it was before.
- Have ClickHouse sync each part the ingester inserts to disk before it
  answers (`fsync_after_insert` and `fsync_part_directory`, set on new tables
  and added to existing ones). A crash of ClickHouse's host could lose parts
  the ingester had already checkpointed and purged from the archive.
- Insert a pending batch that names a recording at its start. A batch saved in
  a tick in which a new recording's replay had delivered nothing yet named it
  there, and after a crash the restart waited for that recording for good.
- Reopen an ingester replay from where it got to, not from its checkpoint.
  A replay that ended while its batch was uncommitted (an insert failed, or
  an application exited) was opened again from the checkpoint, and its
  records were inserted twice with the next batch.
- Write the queued rows of a table taken out of `tables.yaml` where it was
  last listed, once two reads of the file in a row miss it. They stayed
  queued for good, and the ingester stopped reading its archive.
- Decode archive messages in the ingester with the checked `try_decode`. A
  trace, metrics or frame message too short for its body panicked the
  ingester, which then crashed on the same batch after every restart.
- Let every archive connect a persistent feed starts run to Aeron's own
  timeout. The lookup gave up first, on its own clock, and dropping an
  unfinished connect closes nothing, so every retry against a slow archive
  left a publication and a subscription open. On the lab, hundreds of them
  overloaded a node's media driver until its archive answered no one. The
  ingester now retries its archive connect instead of exiting, so its node
  starts recording as soon as the archive answers.
- Keep the ingester's `ClickHouse` connections between inserts
  (`ClickHouse::pooled`). They had been closed after every request, so each
  cross-region insert paid an extra round trip. A request whose kept
  connection the server closed is sent once more, which is safe because every
  insert is deduplicated. A backtest's queries still use a connection each.
- Raise the lab's `book_update` and `tick_to_trade` `slower_than` to 5 s,
  from 100 ms and 50 ms. Both traces start at the venue's own timestamp, so
  the venue's lag and a cross-region feed put a normal total at 65 ms to 1 s,
  and nearly every trace was published as slow. `just verify` now fails when
  more than 1% of either is published.
- Stamp `record_value` rows with the persist clock, so a replay or a backtest
  records simulated time rather than the wall clock.
- Re-read `tables.yaml` in `Persist::poll` only when its length or
  modification time changed, and re-apply the switches without copying them,
  so a steady loop allocates nothing once a second.
- Print `ergon_runtime::Error::Config` as `config: …`, not `tables.yaml: …`:
  it also reports `streams.yaml`, timer and simulation errors, and a
  `tables.yaml` error now names the file.
- End a runtime timer wheel's poll at `i64::MAX` with a 1 ns tick. The tick
  counter overflowed: a release build polled forever, a debug build panicked.
- Collect the complete pending archive batch before inserting after a restart,
  so partial replay delivery cannot consume its deduplication token.
- Let fixed-text decoder helpers yield to schema field names at every depth.
- Respect fixed fields' `sinceVersion` when ingesting padded message and group blocks.
- Pause and resume every kind cluster node, including workers.
- Restore the Full Car generated-source budget to 450,000 bytes by sharing
  wire-dimension validation, including when optional string adapters are enabled.
- Preserve complete histogram samples between recording and polling, and
  publish overdue counters and gauges during continuous histogram traffic.
- Flush a standalone persistence writer's final elapsed histogram window and
  check archive replay positions before flushing historical windows.
- **The cluster crate enabled ergo-sbe's default `fancy` feature on both
  the runtime dependency and the build dependency.** That pulls miette's
  graphical renderer into the cluster build and changes no codec API.
  `default-features = false` is set on the workspace dependency both edges
  inherit. Cargo 1.95 rejects that key on an inherited dependency, and
  leaving either edge on its own defaults keeps `fancy` through feature
  unification.
- **The gated 10k encode and decode benchmarks had no per-function
  correctness check.** Both arms now share one per-message body with an
  untimed preflight: the encode buffers match byte for byte, and the decode
  totals equal 10_000 times the single baseline message. A file-level assert
  no longer counts as that check.
- **`verify` walked repeating groups at the compiled block length and
  version 0.** A wire block longer than the schema was rejected because the
  next tail was read out of the extension padding, and a nested group whose
  wire block length was too short for its fields was accepted. The walk now
  uses the dimension header's block length and the frame's acting version,
  and rejects a short block the same way the group constructor does.
- **A nested fixed-stride group reached through a warm parent entry accepted
  `blockLength` shorter than its required fields.** `wrap_trusted` skipped
  the check `wrap` already applied, so a frame with `numInGroup` 1 and a
  zero block length panicked in `bulk_decode_into` (or read past the buffer)
  from safe code. Both constructors now share that rejection.
- **A `rust_decimal::Decimal` encoded into a decimal composite with a
  constant exponent kept its own scale.** The built-in `TryToSbe` passed the
  unscaled mantissa through, so `1.5` into a `mantissa × 10⁻⁹` composite
  encoded as `0.000000015`; only values whose scale already matched the
  exponent came out right. It now rescales to the exponent, and a value that
  cannot be represented exactly (more digits than the exponent allows, or out
  of `i64` range) is an error, never rounded. This affects every domain
  setter of such a field under
  `with_domain_type(…, "rust_decimal::Decimal")`, and the impl the
  `DomainImpl::Manual` doc snippet suggests.
- **A `UTCTimestamp` field's `timeUnit` was ignored by the built-in
  `chrono::DateTime<Utc>` conversion.** It always assumed nanoseconds, so a
  `timeUnit="second"` field was written as nanoseconds and a millisecond field
  decoded 10⁶× too early. The domain accessors now convert between the
  field's unit and the nanoseconds the conversion works in, at message level,
  in group and nested group entries, and in domain DTOs. A value more precise
  than the field holds is an error, never truncated. Under
  `DomainImpl::Manual` the impl still receives the field's own unit.
- **An invalid `valueRef` built silently.** A malformed reference, an unknown
  enum, an unknown valid value, a non-enum target, or an enum other than the
  field's own now fail the parse, as they do in sbe-tool. Before, the decoder
  kept only the part after the dot, so `valueRef="Other.Buy"` on a `Side`
  field returned `Side::Buy`. A primitive constant field may still reference
  an enum whose encoding type matches its own. A literal constant belongs in
  `constantValue`, not `valueRef`.
- **An included file whose root is `<message>` was dropped silently.** sbe-tool
  splices such a message into the schema; ergon produced no codec and no
  error. It is now a parse error that tells you to declare the message in the
  root schema.
- Keep the ingress channel's settings when the cluster client follows a
  REDIRECT or a new leader. It rebuilt the channel as
  `aeron:udp?endpoint=<leader>`, dropping `term-length`, `mtu` and every other
  setting, so the message limits could change after a failover. Now only a
  unicast endpoint is replaced; IPC and multicast channels are re-added
  unchanged, as Java does.
- Apply a cluster `SessionEvent(CLOSED)` that arrives with or after a
  `NewLeaderEvent`; it was dropped, and a `NewLeaderEvent` could revive a
  closing session.
- Print cluster channel URIs into a buffer of Aeron's 4096-byte URI limit and
  refuse longer channels: Aeron's printer wrote past the end of the fixed 512-
  or 256-byte buffer when a channel or endpoint was longer.

## [0.1.29] — 2026-09-19

### Fixed
- **`acting_version()` / `acting_block_length()` still collided wherever a
  type owned tails, so those schemas did not compile.** The 0.1.28 fix covered
  the group-entry decoder only. The ordered lane emitted both names
  unconditionally on every wrapper, and the message decoder and memoized
  wrapper did the same, so a group or var-data named `actingVersion` /
  `actingBlockLength` (at message or entry level), or an entry field of that
  name beside entry tails, failed with E0592 or E0015. Every decoder, stage and
  ordered wrapper now emits them through one helper that yields to a schema
  name on that type. The header values stay on `get_metadata()`.
- **`with_external_sbe_rt` against a runtime owner without groups or var-data
  did not compile.** 0.1.27 and 0.1.28 sized `EntryInfo`, `Ordered` and
  `OrderedFixed` to the emitting schema, but a runtime owner cannot see the
  schemas of the modules that share it. A generated `sbe_rt` is a whole runtime
  again, so one shared runtime means one set of types:
  `consumer::sbe_rt::EntryInfo` *is* the owner's, as it was before 0.1.27.
- **`<field>_as_str` yields to a sibling tail of that name**, not only to a
  fixed field, through the same helper as `<group>_count` / `<field>_len`.
- Group-entry `acting_version()` / `acting_block_length()` yield when a field
  already owns that name, matching the count/len rule. A schema with
  `actingVersion` / `actingBlockLength` on an entry compiles; the field
  accessor keeps its name.
- Ordered `fixed` / `try_fixed` callbacks yield independently. A first tail
  named `tryFixed` keeps `fixed()`. A first tail named `fixed` also owns
  `try_fixed()` (the try-visit), so both callbacks yield and callers read
  fixed fields before `ordered()`.
- Memoized **and group-entry** var-data getters re-check the schema maximum
  even when `encoded_length()` / the iterator has already published a
  bounds-valid end. Over-max payloads are `InvalidVarDataLength` on the base
  lane, a cold getter, and a warmed one.
- Domain `None` for a `with_all_enums_as_option()` (or `with_null_as_option`)
  enum writes `NullVal` into a reused buffer, including nested-entry DTOs.
  Flat-group `bulk_add_domain` and `to_wire_entry` honour `Option<Enum>` the
  same way. A versioned required enum `None` writes `NullVal` rather than
  skipping the write.
- `*_as_str` / `*_as_str_unchecked` yield per name: a sibling `noteAsStr`
  does not drop `note_as_str_unchecked`.
- The gated `ergo-sbe_ordered` Car decode arm walks recursively with ordered
  callbacks at every tail and observes the same fields as the staged
  consuming arm. `fairness_policy_test` rejects staged `.into_*` anywhere in
  that arm.

## [0.1.28] — 2026-09-15

### Added
- **The ordered lane recurses.** `ordered()` is now generated on any decoder
  that owns tails, not only the message decoder: a group entry carrying its own
  groups or var-data has one too, so a walk keeps a single spelling all the way
  down instead of switching to the staged API at the entry boundary. A nested
  `ordered()` walk is the completion the parent visit owes. Entries with no
  tails of their own have nothing to order and get no lane.
- The staged `into_*` iterator is unchanged and still generated beside it. For a
  fixed-stride nested group it remains the better tool — an `ExactSizeIterator`
  you can `break` out of, still reaching the next tail by arithmetic. The lane
  adds a spelling rather than replacing one.

### Changed
- Nested `ordered()` walks return the parent visit's completion directly
  (`sbe_rt::IntoEntryComplete`); `.done()` is only to unwrap the staged
  complete. The message chain ends on `encoded_length_with_header()` /
  `as_bytes_with_header()`, like encode.
- The ordered lane is `sbe_rt::Ordered<S>` wrapping each staged stage, instead
  of a parallel `*DecoderOrdered*` type per tail. Message-level `fixed` takes a
  `{Name}DecoderFixedView` (fixed fields only) and consumes into
  `Ordered<OrderedFixed<Decoder>>`. Entry-level wrappers no longer emit `fixed`:
  the parent callback already holds the entry. Unprefixed group/var-data methods
  return `DecodeError` so closures infer; `try_*` keeps a custom `E`.
- `EntryInfo` for dynamic groups is filled from the attached group decoder the
  staged walk already opened. The discarded `__sbe_*_dim` wrap is gone.

### Fixed
- A group entry with a field literally named `ordered` no longer collides with
  the new entry-level lane. Group entries are not renamed against the static
  reserved list, unlike message and memoized decoders, so such a field already
  owns `ordered()`; the lane yields and is simply not generated for that entry,
  matching how `<group>_count` / `<field>_len` yield to a colliding sibling.
  A converted field named `ordered` (`ordered_wire` / `ordered_as`) no longer
  suppresses the lane. Covered by `ordered_lane_test`.
- A first tail named `fixed` no longer emits duplicate `fixed()` methods: the
  message-level callback yields, and the group/var-data visit keeps the name.
  Last tail named `done` is a different type from `done()` on the complete
  stage. Covered by `ordered_lane_test`.
- The entry-level lane's `group(&mut self, F)` / `try_group` no longer declared
  a default on their `R` type parameter. Function generic parameter defaults
  are not valid Rust (`invalid_type_param_default`, deny-by-default); every
  group whose entries carry their own tails failed to compile. `R` is always
  inferred from the callback's return expression, so the default was dropped
  rather than replaced.

## [0.1.27] — 2026-09-12

### Added
- **Ordered decode lane — `decoder.ordered()`.** One callback per tail, in
  wire order, with the same spelling at every *message-level* tail (nested
  groups inside an entry keep the staged spelling): `fixed(|&Decoder|)`, then
  `group(|entry, EntryInfo|)` for each group, then `field(|&[u8]|)` (or
  `field_as_str(|&str|)` where the schema declares a text encoding). `done()`
  returns the staged complete stage, so extent and full-frame helpers stay
  reachable. It is a façade over the staged stages and owns no cursor, so the
  entry traversal and compile-time tail ordering come from there rather than
  being re-implemented; entries carrying their own tails still return their
  completion from the callback, which is what keeps the walk one-pass. It is
  free over the staged lane for fixed-stride groups, and costs one extra
  dimension-header read per *dynamic* group per message — never per entry, and
  never a scan — because a visit closure exposes no handle to ask for the
  count. The staged lane stays the floor for all-dynamic messages.
- `sbe_rt::EntryInfo { index, count, block_length }`, handed to every ordered
  entry callback, with `is_first()`, `is_last()` and `remaining()`. Both
  `count` and `block_length` come from the group's dimension header, so neither
  costs a scan. A group's total *byte* length is deliberately absent: for
  entries with tails it is only settled by traversing them.
- **Fixed-stride groups decode as real iterators.** A group whose entries carry
  no tails of their own has a known stride, so `into_<group>()` hands back an
  iterator: `for entry in &mut iter`. It implements `Iterator` only for
  `&mut Iter`, never for `Iter`, so `for entry in iter` does not compile and a
  loop cannot silently drop the rest of the message. Also `ExactSizeIterator`
  and `FusedIterator`. The iterator does not have to be drained — `finish()`,
  or the next tail's `into_*` / `skip_*` called straight on it, skips the rest
  by arithmetic.
- Non-advancing `<group>_count()` and `<field>_len()` accessors, generated at
  every decode location: the base decoder, `.memoized()`, group entries, and
  each staged stage that precedes the tail. Both return `Result<usize>` and
  yield `0` when the tail is absent at the acting version, so sizing a `Vec` or
  logging a count no longer costs a stage.
- `iter.remaining_entries()` and `iter.entry_block_length()` on the staged
  fixed-stride iterator. The ordered lane fills `EntryInfo` from these for
  fixed-stride groups, so that shape costs nothing over the staged walk.

### Changed
- **`ordered` is a reserved decoder method name again.** 0.1.26 removed the
  mutable ordered lane and, with it, the rename that a schema field named
  `ordered` used to take; the new callback lane puts `ordered()` back on the
  base decoder, so such a field is once more generated as `ordered_field()` —
  the same rule that has always applied to `memoized`. The *mutable* lane
  (`{Name}OrderedDecoder`, runtime `OutOfOrder` checks) stays deleted; the new
  lane enforces order through types, not at runtime.
- The `ergo-sbe_ordered` benchmark arm now exercises the ordered lane. It and
  `ergo-sbe_consuming` were both measuring the staged API, so the two labels
  reported the same thing; `fairness_policy_test` now asserts each arm uses its
  own lane.
- Groups whose entries carry their own groups or var-data keep the visit
  closure, and now that is a deliberate split rather than an accident of
  history. Such entries have no stride, and `Iterator::next` cannot learn where
  an entry ended from the entry it already handed away, so an iterator would
  have to measure every entry before yielding it and the caller's own walk
  would traverse it again. The closure returns the entry's completion, and that
  *is* the next cursor. One pass, both shapes.
- `absent_tail_length` is now the single predicate for "this tail is not on the
  wire at this version", shared by all four locations that emit a count or
  length accessor; the memoized lane no longer carries its own copy.
- `sbe_rt::EntryInfo` is emitted only for schemas that declare at least one
  group (and unconditionally in shared-module mode, where one module's runtime
  serves its siblings). It is reachable only through the ordered lane's group
  callbacks, so a group-less schema was carrying it as dead code.
- `extended_optional_enum_nullify` now decodes the optional composite's counter
  alongside the two enums, on both arms. The enum-only form was not gateable:
  both codecs compile those two-byte loads to the same shape, so the true ratio
  is ~1.00 and the measured one was decided by code placement rather than codec
  work — a 2.6% change in unrelated generated code in the benchmarks crate moved
  ergon's arm 37% while the decoder's generated source stayed byte-identical.
  Reading the composite adds equal logical work to both arms and leaves the
  ratio near 0.78, with enough margin that placement drift cannot flip it.

### Fixed
- **`<group>_count` / `<field>_len` could collide with a sibling accessor and
  produce a module that did not compile.** A schema declaring a group `orders`
  beside a field `ordersCount` emitted `orders_count()` twice (E0592), at
  message, memoized and group-entry level alike. Sibling *tails* collided the
  same way: a group `fills` beside a group `fillsCount`, or a var-data `blob`
  beside a var-data `blobLen`. These names are derived from the schema, so they
  cannot live in the static reserved list that drives field renaming. The
  existing accessor now wins and keeps its name — renaming it would change an
  accessor that worked before these convenience methods existed — and the
  convenience accessor is not generated for that tail. Consuming stages carry
  no fields or sibling tails, so they still get it. Covered by
  `reserved_name_clash_test::tail_accessors_yield_to_colliding_field_names`,
  which compiles a schema containing all three collision shapes.
- **`finish()` / `skip_*` on a fixed-stride group stepped over each unread
  entry.** Every entry occupies the acting block length, so the group's end is
  `count * block_length`; the loop was doing a bounds check per entry for a
  result that one multiply gives. Groups whose entries carry tails still walk,
  because without a stride there is nothing to compute.
- The fixed-stride group decoder gained the `size_hint` that its
  `ExactSizeIterator` implementation already implied; it was returning the
  default `(0, None)`.
- The codegen matrix's iterator-surface probe bound its message walk to `_`,
  so a decoding failure in the walk could not fail the test. It now propagates
  with `?`; verified by injecting a truncated buffer, which the bound-to-`_`
  form passed and the fixed form fails.
- `EntryInfo::remaining()` underflowed for a zero-`count` value. `EntryInfo` is
  a public struct, so an independently constructed one could panic in debug
  builds; it now saturates. Its three queries also carry `#[must_use]`, since
  discarding a pure position query is always a bug.

## [0.1.26] — 2026-09-07

### Added
- Var-data declared with `characterEncoding="UTF-8"` or `"ASCII"` now gets a
  `*_as_str(&str)` encode-side setter, at both message and group-entry level,
  alongside the existing `*_as_str` decode-side accessors — writes through
  the checked byte setter after validating ASCII where the schema requires it.

### Changed
- Sequential decode is the staged `into_*(|entry|)` / `skip_*` chain — the
  encoder dual, compile-time order, no `&mut`.

### Fixed
- `characterEncoding` spelling variants legal under the SBE spec —
  `US-ASCII` and `UTF8` (no hyphen), used by real schemas including this
  repo's own Aeron cluster codecs — were silently dropped by message-level
  decode, the memoized decode lane, and group-entry decode, though the
  staged-consuming lane already recognised them. Message-level decode,
  group-entry decode, the memoized lane, and the new encoder now classify
  `characterEncoding` through one shared function so they can't drift again.

### Removed
- The mutable ordered decoder lane: `{Name}Decoder::ordered()`,
  `{Name}OrderedDecoder`, `AnyMessage::into_<name>_ordered()`, and
  `DecodeError::OutOfOrder`. Sequential decode is the staged
  `into_*(|entry|)` / `skip_*` chain; random-access and `.memoized()` remain.

## [0.1.25] — 2026-09-06

### Added
- **Four decoder lanes, generated for every message with groups or var-data:**
  random access (`try_decode`, any order, `Sync`), staged (`into_*(|entry|)` /
  `skip_*`, compile-time order), mutable ordered (`.ordered()`, one
  cursor, runtime order checks), and memoized (`.ordered()`'s sibling —
  `decoder.memoized()` consumes the base decoder and returns a progressive
  `Cell`-backed tail-boundary cache, so repeated or out-of-order tail reads
  walk each boundary at most once). A message with no groups or var-data gets
  only the base decoder — there is no tail to memoize or order, so
  `.memoized()` / `.ordered()` are not generated and `AnyMessage` exposes only
  `into_<name>()` for it.
- `AnyMessage::into_<name>_memoized()` / `into_<name>_ordered()` land directly
  in a lane without an intermediate base-decoder conversion.
- Var-data declared with `characterEncoding="UTF-8"` or `"ASCII"` now gets
  `*_as_str` / `*_as_str_unchecked` text accessors on **group entries**, not
  only at message level — the same helper that already existed for message
  var-data, shared rather than duplicated.
- `GenerationConfig::with_encode_version(version)`: the encoder writes
  `version` in the header and omits members introduced after it, while the
  decoder still reads every version in the schema. Useful for producing older
  wire versions for peers that have not upgraded. It is rejected at generate
  time when it would drop members alongside domain objects or conversions.
  Covered by a versioned-L3 fixture (v0–v3) that is byte-compared against
  sbe-tool at every acting version.
- Cluster Callgrind instruction probes (`just bench-cluster-instructions`) —
  mechanism-level evidence for cluster session codec hot paths, alongside the
  existing SBE probe lane.

### Fixed
- **`<data characterEncoding="...">` was silently dropped by the schema
  parser.** SBE allows `characterEncoding` directly on `<data>`, not only on
  the referenced composite's `varData` member; real schemas use the element
  form. The parser only read the composite-member form, so a declared-text
  field silently downgraded to raw `&[u8]` with no `*_as_str` accessor at all
  — a wire-compatible but API-degrading bug with no error to signal it.
- **A truncated or undersized fixed-stride group could be read out of bounds
  through the mutable ordered decoder.** The fixed-stride branch of
  `.ordered()`'s group entry validated `count * blockLength` but never
  `blockLength` against the fixed fields' minimum readable extent — a
  `blockLength` smaller than a required field's offset let a safe getter read
  past its entry. Both the truncation and the undersized-stride case are now
  rejected before any entry reaches a caller callback, matching the
  random-access lane's existing checks.

### Removed
- `GenerationConfig::with_memoized_tail_offsets` and
  `with_compact_tail_offsets` — memoization is now a runtime lane
  (`decoder.memoized()`) rather than a generation-time, module-wide knob, so
  the choice moves to each call site instead of being fixed for the whole
  module. Compact `u32` tail-offset storage was measured and dropped: it saved
  a little decoder memory but cost more instructions on the cache's hot
  primitives and was materially slower under LTO.

## [0.1.24] — 2026-08-31

### Fixed
- **Domain objects + domain types generate compilable code again.** Fifteen
  codegen defects made `with_domain_type` / `with_manual_domain_type` unusable
  alongside `with_domain_objects` for most field shapes. Every one produced a
  generated module that failed to compile, so no wire format changed and no
  working code can have depended on the old behaviour.

  Reported against 0.1.22 and 0.1.23, then found while root-causing it:
  - A `sinceVersion > 0` **composite** wrapped `try_*` in `Some(..)`. `try_*`
    on a versioned field already returns `Result<Option<T>, E>`, so the DTO
    built `Option<Option<T>>` for an `Option<T>` field.
  - The same double-wrap on a `sinceVersion > 0` **primitive scalar**.
  - A **fixed array** matched by a selector generated converters against the
    element type (`u8`) instead of the array type (`[u8; 4]`).
  - An **array or optional scalar** with a conversion read the raw flyweight
    getter under its pre-rename name, though a conversion renames it `*_wire`.
  - An **array or optional scalar** DTO kept the wire type but wrote through
    the domain `try_*` setter, which takes the domain type.

  Found by the combination matrix below:
  - A **versioned array** was treated as `Option`-returning, though an array
    accessor returns a zero-filled array, never `None`.
  - A **constant** field matched by a selector generated conversion accessors
    against a `*_wire` setter that constants do not have.
  - `Display` for a **group-entry composite** with a domain type re-derived the
    conversion and fed `Option<T>` to `TryFromSbe<T>`; it now goes through the
    generated `try_*` accessor.
  - `Display` for a **converted primitive** read the raw getter under its
    pre-rename name, at message and group-entry level.
  - Conversion-only `*_as` accessors did not `Option`-unwrap a versioned or
    optional raw getter, at message and group-entry level.
  - `with_all_enums_as_option` makes enum accessors return `Option<T>`, which
    neither the `*_bool` accessors nor the domain DTO accounted for.
  - An **optional bool enum** produced a group-entry accessor that matched
    `Option` against a plain `BooleanType`.
  - An **optional enum with a domain type** was treated as `Option`-returning;
    an optional enum carries `NullVal` as a variant, so its accessor is plain.
  - `null_as_option` **combined with an enum domain type** left `try_*`
    disagreeing with the accessor it delegates to, in both directions.
  - Group-entry and message-level boolean accessors disagreed on when they
    return `Option`; both now do so only when version-gated.

  Most shared one root cause: the same decision — "is the domain type applied
  to this field?", "is this accessor `Option`-returning?" — was made
  independently in several places and drifted apart. Those now live in single
  predicates (`conversion_helpers::dto_domain_type`,
  `converter_impls::is_optional_domain_field`).
- `ConversionSelector::FieldPath` selects fields again. It is documented as the
  primary selector form and validation accepted it, but codegen matched only
  `NamedType` and `SemanticType`, so every `FieldPath` selector was a silent
  no-op. Group and nested-group fields extend the path with their group names
  (`"Message.group.field"`).
- Documented selector precedence is enforced: `FieldPath` beats `SemanticType`
  beats `NamedType`. Resolution was "first selector in registration order", so
  precedence silently depended on call order.
- Domain types configured for **enum** and **set** fields reach the DTO.
  `try_*` accessors were generated for them all along, but the domain-DTO
  generator ignored them and emitted the raw generated type. Boolean enums are
  unchanged — they still materialise as `bool`.
- Scratch-crate test helpers no longer force `--offline` / `CARGO_NET_OFFLINE`,
  and they strip that flag when inherited from `cargo llvm-cov`. Nested
  compile-and-run crates can download extra transitives (`bit-set`, `ahash`)
  that are not in the workspace lock — the cause of red `test`/`coverage`
  jobs on 0.1.19–0.1.22 and Dependabot PRs.
- The coverage job timeout is 120 minutes (was 30). Instrumented
  compile-and-run tests cannot finish in half an hour; the 0.1.23 coverage
  job was cancelled mid-run.
- Nightly `cargo fuzz` pins `x86_64-unknown-linux-gnu`. cargo-fuzz's Linux
  default is musl+ASAN, which fails on a minimal nightly without that target.
- Group-dimension IR extraction matches the required `blockLength` and
  `numInGroup` members by exact name. Padding fields such as `account`,
  `countPadding`, or `blockLengthPadding` no longer overwrite the layout.
- Generated group-entry `Display` formats a domain-mapped set through
  `try_*` and `{:?}`. The raw accessor is `*_wire`; calling `self.<field>()`
  failed to compile.
- `poll_connect_until_done` idles with work-count 0 while the handshake is
  still in progress. `AsyncClusterConnect::poll`'s `true` means more polling
  is needed, not that Aeron performed work; a positive count reset backoff
  and pinned a core.
- `parse_ingress_endpoints` rejects empty comma-separated entries (leading,
  trailing, or repeated commas) instead of skipping them.
- `cluster_decode_session_event` carries a documented no-LTO-only allowance of
  1.05 (LTO stays at 1.01, where ergon measures 0.99). Its no-LTO arm is a
  memory-bound decode that walks either side of parity — 1.0403 and 1.0444 on
  consecutive quiet-machine runs, matching the 1.0406 already recorded in the
  pair's history. This is an allowance for a placement-sensitive tie, not a
  proof that nothing regressed; above 1.05 the gate fails as before.
- `write_module_set` restore after a mid-commit failure replaces an
  already-promoted destination (Windows-safe) and surfaces restore errors
  rather than discarding them. Successful publication fails if a `.bak`
  backup cannot be removed.

### Added
- **Codegen combination matrix** (`sbe/tests/codegen_matrix_test.rs`). Every
  field shape x `sinceVersion` x mapped/unmapped cell, at all three codegen
  locations (message, group entry, nested group entry), crossed with every
  `GenerationConfig` combination that selects a distinct codegen path — and
  each case **compiles** the generated module rather than inspecting its
  source. Source-string assertions were what let the defects above ship: they
  stay green while the module fails to compile.

  The fixture is generated by `scripts/regenerate-codegen-matrix-fixture.py`,
  whose `SHAPES` list is the single source of truth; the test recomputes the
  same cross product and fails when the fixture falls behind, and `--check`
  (wired into `just preflight`, `just test`, and CI) fails on drift.

### Changed
- **Breaking (generated API):** the decoder's boolean getter is `try_{field}_bool`
  at every shape and location. Message-level code named the `Option`-returning
  one `{field}_bool` while group entries named both shapes `try_*`, so the same
  schema shape had two different accessor names depending on where it sat —
  which is what let the domain DTO call an accessor that did not exist.
  `{field}_bool` is now unambiguously the *encoder setter*; `try_` marks the
  fallible decode. The only remaining shape difference is the return type:
  `Result<bool, _>`, or `Result<Option<bool>, _>` when `sinceVersion > 0` makes
  the field absent on older wire versions.
- **Breaking (generated API):** conversion-only `{field}_as::<T>()` returns
  `Result<Option<T>, _>` for a `sinceVersion > 0` or optional field, matching
  the domain-typed `try_*` accessors. It previously generated a body that fed
  `Option<W>` to `TryFromSbe<W>`, so no compiling code can depend on the old
  signature.
- Hook `FieldInfo::rust_type` for groups and var-data is a real type
  (`LevelsDecoder`, `&[u8]`) instead of the sentinels `"group"` / `"data"`.
  `FieldInfo::kind()` returns `FieldKind` (`Fixed`, `Group`, or `Data`).
- Consumers are advised to build with `lto = true` / `codegen-units = 1`.
  Generated codecs are many small `#[inline]` methods whose inlining only fully
  collapses across codegen units; `sbe/BENCHMARKS.md` and the book's benchmark
  chapter now document the recommendation. Benchmark methodology no longer
  describes the LTO profile as informational — both profiles have been blocking
  gates with the same literal `1.00` ceiling.
- Hook `FieldInfo` carries a `kind: FieldKind` field recorded when the field
  is built. `kind()` returns it instead of inferring from `rust_type` suffix
  and substring checks, so renaming a generated type cannot silently change a
  field's meaning.
- `just check-local`, `just check-products`, and `just test` no longer run the
  `ergo-aeron-cluster` suite twice: the crate is excluded from the workspace
  run that already covers it explicitly, and its doctests run once via
  `cargo test -p ergo-aeron-cluster`. Same for CI's workspace doctest job.
- CI, `just test`, and the local product gates run
  `cargo test -p ergo-aeron-cluster` (integration tests, compile-fail API
  suite) rather than `--lib` only.
- Generated public-API freeze records trait associated-type bounds,
  defaults, generics, where-clauses, and semver attributes.

## [0.1.23] — 2026-08-28

### Fixed
- Generated `Display`/`Debug` bodies no longer assume a `with_domain_type`
  target implements `Display`. Domain values format with `{:?}`, so a domain
  type only has to be `Debug`.
- `with_display_debug(false)` now also omits the group entry decoder's
  `Display` impl, which was emitted unconditionally.

### Changed
- **Breaking:** optional var-data accessors (`into_*_as_compact_str`,
  `into_*_as_smol_str`, `into_*_as_bytes`) are emitted only when the
  *generator* was built with the matching `compact_str` / `smol_str` / `bytes`
  feature — reverting the 0.1.x switch to gating them on the consumer's own
  features. That gate was a `#[cfg(feature = "…")]` in the emitted source,
  which resolved against the consumer crate's feature table; consumer crates do
  not declare features by those names, so the accessors were unreachable dead
  code in every generated module. Enable the feature on `ergo-sbe` in both
  `[build-dependencies]` and `[dependencies]`.
- The generated public-API snapshots no longer record those accessors, since
  their presence now depends on the generator's feature set.

## [0.1.22] — 2026-08-27

### Added
- Generated observers named by T-1 (`after_this_message`, message/group
  `min_readable_fixed_extent`, `MessageHeader` peeks, `schema_id_from_header`,
  domain `to_wire_entry`) carry specific `#[must_use]` diagnostics.
- `StaticCredentials::new` accepts `impl Into<Vec<u8>>` (moved `Vec`, slice,
  array, embedded NUL, invalid UTF-8) with byte-identical connect and challenge
  credentials. `from_utf8` remains the text convenience.
- Poll-driven connect recipe in rustdoc, `cluster/README.md`, and the session
  builder book chapter: `SessionBuilder` → `connect_async` → `default_idle` →
  `poll_connect_until_done` → `finish`. `AsyncClusterConnect` is a poll-driven
  Aeron state machine, not a Rust `Future`.
- Group-dimension composites accept unsigned `u8`/`u16`/`u32`/`u64` members
  (both endians, reordered offsets, padded composites over 32 bytes). Parser
  rejects signed/optional/array/missing/overlapping/out-of-bounds members.
  Over-limit counts and non-representable 32-bit `u64` wire values are typed
  range errors.
- Unpublished workspace crate `ergo-aeron-cluster-test-harness` owns the Java
  ClusterLauncher, harness tests, and examples. Release automation unpacks
  `ergo-aeron-cluster` with every advertised feature and fails closed.

### Changed
- **Breaking:** `ergo-aeron-cluster` no longer advertises a `test-harness`
  feature or `test_support` module. Depend on `ergo-aeron-cluster-test-harness`
  in this repository.
- `AnyMessage::decode_frame` rejects every `frame_len` in `0..HEADER_LENGTH`
  before reading header fields, even when the backing slice contains a later
  valid header.

## [0.1.21] — 2026-08-24

### Added
- `GenerationConfig::with_manual_domain_type` and `with_domain_type` now emit
  correctly `Option`-wrapped `try_*` accessors for optional (or
  `sinceVersion`-gated) domain-typed fields inside repeating groups, matching
  the message-level behaviour. Generated `Display`/`Debug` for group-entry
  domain-typed `Primitive`/`Enum` fields now shows the converted domain value
  instead of failing to compile.

### Changed
- **Breaking:** `BuildError` is `#[non_exhaustive]`. `Io(std::io::Error)` is
  now `Io { action: &'static str, path: PathBuf, source: std::io::Error }`,
  naming the exact attempted step and destination instead of a bare I/O
  message.
- `generate_multi_to_dir` / `write_module_set` publish a complete generated
  module set as one all-or-nothing unit: every module is staged to a unique
  temp file before any destination is touched, and a mid-commit failure
  restores every pre-existing destination and leaves no temp/backup debris.
  A cleanup step that itself fails is reported in preference to the failure
  that triggered it, so a rollback never reports itself clean while leaving
  debris. Deleting the backups of a *successful* commit is past the point of
  no return and does not roll back: every backup that can be removed is
  removed, then the publication fails naming the first that could not.
- `GenerationConfig::with_keyword_append_token` is validated at `generate()`:
  an empty or invalid token now returns
  `GenerateError::InvalidConfiguration` instead of generating uncompilable
  Rust.
- **Breaking:** `SessionBuilder::ingress_endpoints` returns
  `Result<Self, ClusterError>` and validates the endpoint map (grammar,
  duplicate IDs, empty entries) at the call site instead of at `validate()`
  or the first `poll()`.
- **Breaking:** `poller::parse_event` returns `Result<EgressEvent, ClusterError>`
  (was `Result<Option<EgressEvent>, ClusterError>` — every decoded branch
  was already `Some`, so `None` was unreachable).
  `poller::parse_leader_endpoint` returns `Result<Option<String>, ClusterError>`
  (was `Option<String>`) — a malformed endpoint map is now `Err` with the
  parser's specific reason, distinct from `Ok(None)` for a well-formed map
  that simply lacks the requested leader.
- Message-bearing `#[must_use]` on pure cluster decision observers:
  `PublicationFailure::is_retryable`/`raw_code`, `ClusterError::is_retryable`,
  `AeronCluster::{cluster_session_id, leadership_term_id, leader_member_id,
  is_ingress_connected, is_ingress_closed, ingress_position,
  is_egress_connected, state}`, `ClusterClaim::position`, and
  `AsyncClusterConnect::{step, is_complete}`.
- **Breaking:** `PublicationFailure` is `#[non_exhaustive]` and gains
  `TooManyParts`, mapped directly from `AeronOfferError::TooManyParts`
  instead of the fabricated sentinel `Other(-100)`. `raw()` is replaced by
  `raw_code() -> Option<i64>` (`None` for `TooManyParts`, which has no real
  Aeron wire code).

- The generated public-API freeze gate (`api/generated/*.txt`) now snapshots
  the canonical semver-relevant surface — struct fields, enum variant
  payloads/discriminants, full fn/method signatures (receiver, generics,
  args, return type, where-clause), associated types/consts, type aliases,
  and cfg/non_exhaustive/repr/deprecated/must_use attributes — instead of
  item names only. A cfg-gated item is now recorded (with its condition)
  instead of being silently excluded from the freeze.
- The `SbeMessage` sealing-trait path is now explicit per-schema state
  (`GenerationContext`, passed by reference into decoder/encoder generation)
  instead of a `thread_local!`. A hook that invokes a nested `Generator`
  (e.g. to emit a companion crate) can no longer leak its sealed path into
  the outer generation's remaining messages.
- `sbe/BENCHMARKS.md` and `book/src/sbe/benchmarks.md`'s "Group encode: LTO
  on and off" section no longer quotes unprovenanced point estimates
  (`414.1 ns`, etc., with no run-id/commit/host and duplicated verbatim
  across both files) as if they were current — it states the qualitative
  ranking and the reproduction command instead.
- Generated domain-object `encoded_length()` / `encoded_length_with_header()`
  rustdoc no longer contradicts `encode()`: `encoded_length()` is documented
  as body-only, and `encoded_length_with_header()` as the exact buffer size
  and return value of `encode()` for both fixed and dynamic messages.
- Generated `{Group}Encoder::wrap`, `ENTRY_BLOCK_LENGTH`, and
  `GROUP_DIM_TEMPLATE` now carry authored rustdoc explaining the standalone
  group framing contract (offset is the first entry, the dimension header is
  the caller's responsibility) instead of a placeholder.

### Fixed
- Domain-object generation for a required (non-`sinceVersion`) boolean field
  no longer panics on an unknown wire discriminant; `try_from_decoder`
  propagates `DecodeError::InvalidBoolean` instead.
- Single-line schema `description` text containing `&`, `<`, or `>` (e.g.
  `Option<u32>`) is now escaped before emission, so it can no longer break a
  downstream `-D warnings` rustdoc build with `rustdoc::invalid_html_tags`.

## [0.1.20] — 2026-08-20

### Added
- `GeneratedModuleSet::into_parts` consumes the set and returns owned modules
  and warnings without cloning generated source.
- `SchemaFile` plus `generate_multi_to_dir` / `generate_multi_to_out_dir`
  generate a shared schema and its consumers in one transaction, using
  supplied module names and watching every resolved include.
- `EncodeError::InvalidAscii { field }` for ASCII fixed-array `*_str` writes.
- Generated public-API snapshots under `api/generated/` (`car_lean`,
  `car_domain`, `multi_schema_shared`) and
  `scripts/check-generated-public-api.sh`, invoked from `just preflight`.
- `GenerationConfig::with_manual_domain_type` for additive Manual
  `TryFromSbe`/`TryToSbe` mappings. `with_domain_type(selector, path)` is
  the two-argument Generated path.

### Changed
- `GenerationConfig::with_domain_type` stays the two-argument
  `(selector, path)` Generated path it has always been. Use
  [`with_manual_domain_type`] for additive Manual impls.
- **Breaking:** `ParseError` is `#[non_exhaustive]`. Root file reads are
  `Io { path, source }`. Include failures are `Include { href, attempted,
  cause }` with typed `IncludeCause` (`Cycle` / `Io` / `NotFound`).
- **Breaking:** `SessionBuilder::ingress_channel`, `egress_channel`,
  `message_timeout`, and `new_leader_timeout` return `Result` and store only
  validated values. `is_ingress_exclusive` and `owns_aeron` setters are
  removed.
- **Breaking:** generated `*_str` exists only for default-ASCII `char` and
  encodings `ASCII` / `US-ASCII` / `UTF-8` / `UTF8`. Unencoded numeric arrays
  and encodings such as GB18030 keep the raw array setter only — schemas
  relying on the old setter for those types need the raw accessor instead.
- Optional var-data accessors (`into_*_as_compact_str`, `into_*_as_smol_str`,
  `into_*_as_bytes`) are always emitted and gated on the *consumer's*
  `compact_str` / `smol_str` / `bytes` feature, not the generator's. Fixes a
  feature leak where the generator's own feature flags decided what a
  consumer crate could see, independent of that crate's own Cargo features.
- Generated rustdoc snippets referencing a `DomainImpl::Manual` field render
  as plain text instead of an ignored `rust,ignore` fence.
- Side-effect-free generated observers (`as_option` / `as_bool`, completed
  tail views/lengths, encoder metadata, group `written`, exact-length
  terminals) carry message-bearing `#[must_use]`.

### Fixed
- One-byte fixed-array getters (`char` / `uint8`) return the bulk-read
  `[u8; N]` instead of reconstructing each element with `from_le_bytes`.
- `parse_with_xsd_validation` accepts schema-declared enum `nullValue`
  (signed and unsigned) and still rejects unknown non-namespaced enum
  attributes.
- `generate_to_dir` emits `cargo::rerun-if-changed` for the root schema and
  every nested or transitive include, so an include-only edit rebuilds.
- Unresolved-type diagnostics name the containing field as well as the
  invalid type.

### Deprecated
- `GenerationConfig::with_error_from_impls` — it converts generated encode/decode
  errors through `format!` and `From<String>`, dropping fields such as `needed`
  and `available`. Implement `From<generated::sbe_rt::EncodeError>` /
  `From<generated::sbe_rt::DecodeError>` instead. Removal is scheduled for 1.0.

## [0.1.19] — 2026-08-19

### Fixed
- A composite member with `presence="optional"` and a schema `nullValue`
  (e.g. a `PriceNull9`-style Decimal's `mantissa`) now decodes as `Option<T>`,
  checked against the wire null sentinel, instead of silently misreading the
  null image as a real value. `with_domain_type` mappings onto such a
  composite (e.g. `rust_decimal::Decimal`) now fail closed with a typed error
  on the null image rather than decoding the sentinel as a huge/wrong number.

## [0.1.18] — 2026-08-18

### Added
- Reject unknown attributes on `<messageSchema>`, `<message>`, `<field>`,
  `<group>`, and `<data>` at parse time, so an authoring typo (`presense`,
  `semanticTpye`) is an error instead of being silently ignored. Namespaced
  attributes (`xsi:*`, `xi:*`, and vendor namespaces such as Binance's
  `mbx:*`) are outside the SBE grammar and still pass.
- Multiple distinct decimal composites (`Decimal64`, `Decimal128`, …) can each
  map to `rust_decimal::Decimal` via `with_domain_type`; each gets its own
  `TryFromSbe`/`TryToSbe` impl.

### Fixed
- `validate_against_sbe_xsd` no longer rejects valid real-world schemas. It
  dropped every namespaced vendor attribute check (`mbx:exponent`) on the
  floor, and its allow-lists omitted `characterEncoding` on `<data>`, `unit`
  on `<type>`, `jsonValue` on `<validValue>` / `<choice>`, and `package` on
  `<types>` — enough to reject checked-in `l3-book` and `binance-spot`
  schemas. Attribute allow-lists are now shared with the parser so the two
  cannot drift apart.
- Domain-typed optional fields no longer produce a type mismatch. An optional
  `rust_decimal::Decimal` composite decodes to a plain `Decimal` (a composite
  has no null image), an optional `chrono::DateTime<Utc>` decodes to
  `Option<DateTime>`, and the encoder setters take the plain domain value.
- A boolean enum mapped to `bool` via `with_domain_type` now emits its
  `TryFromSbe`/`TryToSbe` impl once per shared module instead of in every
  consumer, which caused a "conflicting implementation" across multi-schema
  modules.

### Changed
- Fixed-only encoders now carry `FieldsState`: `as_bytes_with_header`,
  `as_body_bytes`, `encoded_length*`, and `into_remaining_mut` exist only
  after `fixed(&FixedFields)`, so a reused buffer cannot publish or pack
  unwritten required fields.
- Cluster bench gate uses the same literal `1.00` ceiling as SBE and requires
  `--run-id` provenance (`just bench-cluster` stamps per-estimate run ids).
- `check-public-api.sh` strips a leading `v` from `baseline_tag` before
  passing `--baseline-version` to cargo-semver-checks.

### Fixed
- Schema `nullValue` / `minValue` / `maxValue` that do not fit the declared
  primitive width are rejected at parse time (no more `256_u64 as u8`).
  Signed enum `validValue`s are compared as signed, so `int8` `-1` is
  accepted when `minValue="-5"` and `maxValue="5"`.
- Maintained cluster decode benches wrap both arms at the body offset without
  extra template/schema/version work on the sbe-tool side.
- Docs-validation temp crates `cargo fetch` before `CARGO_NET_OFFLINE=true`,
  so a clean CI runner is not missing optional transitive crates.

## [0.1.17] — 2026-08-14

### Changed
- **Breaking:** writing a message's tail now requires the fixed block first.
  `wrap*` returns `{Msg}UnfixedEncoder`; groups and var-data are reachable only
  after `.fixed(&{Msg}FixedFields { … })`. "Tails before the fixed block" is now
  unrepresentable rather than a runtime hazard.
- Gate the fixed-stride group proof APIs (`add_checked`, `start_entry`,
  `complete`, `EntryComplete`) on groups that actually have a fixed stride. A
  dynamic entry with a var-data tail can no longer claim completion.
- `ClusterError` is `#[non_exhaustive]`, so adding a variant is not a breaking
  change for downstream matches.
- Decode errors keep their UTF-8 cause in the error chain instead of flattening it.
- Malformed XML and schema-invalid SBE now surface as distinct errors.

### Added
- Schema `description` text is emitted as rustdoc on messages, fields,
  composite members, enum variants, group entries, and var-data.
- `#[must_use]` on pure generated observers — getters, set predicates, raw
  enum/set values, and metadata queries — so discarding them is a warning.
- Multi-schema generation validates that same-named shared types have identical
  wire fingerprints, and rejects duplicate or non-identifier module names.
- Fail-closed benchmark evidence packaging (`scripts/package-bench-artifacts.sh`)
  wired into both the release workflow and `just release`.
- sbe-tool parity benchmarks for optional-enum nullification and
  group-with-data, both gated at the strict `1.00` ceiling.

### Fixed
- Optional primitive fields write the schema null image when set to `None`,
  including fixed arrays and nested optional composite members.
- Cluster egress decoding runs through one canonical fail-closed path; short,
  truncated, and invalid-text frames report the real error instead of a
  misleading timeout.
- `parse_event` reports the real template id for decoded-but-unprojected frames
  instead of `0`.
- Session builder timeouts reject zero, sub-millisecond, and overflow values,
  and each timeout field tracks its own error so one cannot mask another.
- `StaticCredentials` and `SessionBuilder` redact credential material from
  `Debug` output.
- Duplicate member IDs in an ingress endpoint map are rejected.
- Generated multi-paragraph rustdoc is emitted one line per `#[doc]`, so it is
  no longer parsed as an indented Markdown code block and compiled as a doctest.

## [0.1.16] — 2026-08-10

### Changed
- Decoder structs use `offset` field (matching sbe-tool naming), replacing internal `base_addr` cache. `read_addr_unchecked` helper removed.
- Generated encoder cursors renamed `offset` for consistency across all decoder/encoder types.

### Added
- Historic ergo regression benchmarks — null-as-option, optional fields, Decimal converters. Gate: `just bench-historic`, `scripts/check-bench-historic.sh`.
- Multi-message/framing documentation: `AnyMessage` dispatch, back-to-back and `remaining()` framing approaches.
- miette error diagnostics page with rendered examples of schema parse errors.

### Fixed
- toml 1.x `FromStr` compat: `toml::Value::from_str` replaced with `toml::from_str` for full-document parsing.
- Matrix test crate missing feature flags (`compact_str`, `smol_str`, `bytes`).
- Book: removed incorrect "Java-style" label from parent-hopping sections.

## [0.1.15] — 2026-08-09

### Added
- **`with_null_as_option(ConversionSelector::named_type("EventCode"))`** — enum fields become `Option<EventCode>`. `NullVal` wire byte → `None`.
- **`with_all_enums_as_option()`** — every enum in the schema → `Option<Enum>`.
- **`as_option()` on every generated enum** — `event_code.as_option()` → `Option<EventCode>`.
- **`nullValue` on `<enum>` elements** now parsed from XML. `<enum encodingType="uint8" nullValue="99">` → `NullVal = 99`.

### Changed
- Crate description: `"Simple Binary Encoding (SBE)..."` for crates.io visibility.
- Group-entry enum fields support `with_null_as_option`.

## [0.1.14] — 2026-08-08

### Breaking
- `SessionState::AwaitingNewLeaderConnection` removed (merged into `AwaitingNewLeader`).
- `ClusterError` now `#[non_exhaustive]`, gains `InvalidTimeout` and `PayloadTooLarge`.
- `AsyncClusterConnect::step()` returns `ConnectStep` (non-exhaustive, wildcard required).
- `GenerationConfig::profile(Lean)` no longer clears explicit conversions/domain-types. Use `.lean()` for a clean baseline.

### Added
- `#[must_use]` on `SessionBuilder`, `AsyncClusterConnect`, `ClusterClaim`.
- `ConnectStep` re-exported at crate root.
- `checked_deadline` helper — zero/overflow timeouts return `InvalidTimeout`.
- `PayloadTooLarge` error with `operation`, `requested`, `maximum` fields.
- Zero-alloc fragmented path via `offer_parts`.
- Feature-gated integrations: `compact_str`, `smol_str`, `bytes`, `chrono`.
- `DomainVarData::CompactStrings`, `SmolStrings`, `BytesCrate` variants.
- `lean()` constructor on `GenerationConfig`.
- Module name validation + path containment.
- `#[must_use]` on codec tail stages.
- FIX SBE conformance suite documented.

### Fixed
- Malformed egress frames (< 8 bytes) → `ProtocolError`, not silently unknown.
- Decode errors propagated on poll path.
- `checked_add` overflow in `offer()`.
- Stale channel parse errors cleared on re-set.
- Benchmark docs match gate: SBE zero tolerance, cluster 0.005.

## [0.1.13] — 2026-08-07

### Breaking
- `SbeMessage` is a sealed trait — only generated types can implement it.
- Staged `EncodedLength` types renamed for stage-just-completed convention.
- Group decoders no longer expose `wrap_with_parent`.
- Safe constructors prove fixed extent and panic if short; `unsafe *_unchecked` skips.
- `From<BooleanType> for bool` removed — use `as_bool()`.
- `EncodeError::BufferTooShort` carries `field`.
- `DomainConversionFailed` carries `reason`.

### Added
- `add_checked` — group entries proven complete at compile time.
- Group decoders poison themselves on malformed entry.
- `#{inline}` on `bulk_decode`, staged finish helpers, domain DTO methods.
- `Error::source` on `EncodeError::Decode` and `VerifyError::DecodeError`.
- `#[must_use]` on consuming stages and `EncodedLength` stages.

### Fixed
- Constant-presence fields no longer count toward readable fixed extent.
- Aeron `try_claim` recipe rewritten against real API.
- Bench batch-decode uses `wrap_unchecked` for equal work.
- Deduped reserved method-name lists.
