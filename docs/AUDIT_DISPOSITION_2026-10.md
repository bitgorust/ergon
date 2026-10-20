# Documentation and benchmark audit — disposition

Review record for the 2026-10 cleanup of unsupported claims, broken
benchmark accounting and unverified recommendations. This is a **review
artifact**: it records what the audit found, what evidence backed each
finding, and what was done with it. It is not a tutorial and not a source
of performance figures. Live figures live in the run artifacts under
`target/md-dpdk-bench/`, `io/benches/results/` and the per-crate
`BENCHMARKS.md` files, each quoted from a named run.

**Verdicts used:** `wrong-aggregation` (figure is real but summarised
under a different definition than the prose claims), `no-artifact` (no run
row contains the figure), `wrong-fact` (contradicted by code or by the
run it cites), `contradicted-by-code`, `quantitative` (directional claim
with no number, no max, no run id), `unsupported` (claim whose evidence
does not establish it).

**Dispositions used:** `rewrite` (keep the measurement, fix the summary),
`correct` (replace with artifact values), `remove` (delete the claim),
`unverified` (label explicitly; never invent a number).

**Status at time of writing.** Stage 1 evidence restoration is in
progress; stage 2 benchmark-validity repairs are open; stages 3 and 4 are
open. Rows whose fix has landed are marked in the tables below where that
is already known. Nothing in this file certifies every historical
experiment, and nothing here establishes that any library meets an
unspecified HFT latency budget.

---

# ergon documentation cleanup disposition

Root: `/Users/mimra/RustroverProjects/project/ergon`. Evidence quotes are artifact rows or source lines as given in the verified inventory. No new measurements.

## 1. Disposition table

### clock

| file:line | claim | verdict | evidence | disposition | exact fix |
|---|---|---|---|---|---|
| `clock/BENCHMARKS.md:58` | Discipline residual table: "median of 5 reps"; c7i `2 \| 2,726 \| 9,443 (1,447–9,443)`, c8g `3 \| 282 \| 1,447 (1,447–2,100)`; c8g/c9g update cost both "max 1.8–2.9 µs" | wrong-aggregation | Six `discipline_summary,residual_at_samples_ns` rows, one 1800 s run per node (n=1801): c7i `1801,3,2726,7019,9443`/`1801,2,685,2058,3950`; c8g `…532…2154`/`…291…1447`; c9g `…282…2580`/`…397…2657`. c7i's range low 1,447 is c8g-b's max. Update maxima c8g 2894/2895, c9g 1759/1840 | rewrite | Per-host ranges across the two nodes: c7i p50 2–3, p99 685–2,726, max 3,950–9,443; c8g p50 2, p99 291–532, max 1,447–2,154; c9g p50 2, p99 282–397, max 2,580–2,657; update maxima c8g 2.89–2.90 µs, c9g 1.76–1.84 µs. Header: "one 1800 s run per node, n=1801 samples, two nodes". |
| `clock/BENCHMARKS.md:64` | c9g row `2 \| 310 \| 1,610 (1,610–2,400)`; c8g max high `2,100` | no-artifact | No `discipline_summary` row has 310, 1,610, 2,400 or 2,100. Real c9g: p99 282–397, max 2,580–2,657; c8g-a max 2154 | correct | Replace with artifact values (c9g p99 282–397, max 2,580–2,657; c8g max 1,447–2,154). |
| `clock/BENCHMARKS.md:13` | `rdtscp` 20.00 (21.08) etc.; read-cost "ordered `now_ns`" column `20.00 (21.08) \| 19.48 (19.79) \| 11.36 (11.67)` (also :44) | wrong-fact | Those figures are `cost,ergon_ticks_ordered` (`20.00,20.94,21.08`). Real `cost,ergon_now_ns_ordered`: `22.73,23.79,23.88` / `21.93,22.33,22.34` / `11.35,11.65,11.65`. `raw_rdtscp` max is 21.11, not 21.08 | correct | Quote `ergon_now_ns_ordered` in the now_ns column (22.73 (23.88) / 21.93 (22.34) / 11.35 (11.65)); or rename the column "ordered ticks read" (costs stay ns). On :13 label as `ergon_ticks_ordered`, or use raw_rdtscp's 20.00 (21.11). |
| `clock/BENCHMARKS.md:52` | Drift "within 0.01 ppm" on all three hosts beside "rate vs kernel scaling \| −0.048 to 0.000 ppm" | wrong-fact | `drift,ergon_now_ns,60,…,-2851,-0.048,raw` (c7i) and `…,3,0.000,raw` (c8g). c9g `drift_secs=0`, no drift rows. The two columns are one measurement | rewrite | c7i "−0.048 ppm at 60 s and 120 s"; c8g "0.000 ppm"; c9g "not measured (drift_secs=0)". Drop or relabel the duplicate "rate vs kernel scaling" column. |
| `clock/BENCHMARKS.md:12` | quanta process disagreement "1.9–6.9 ppm" (also :56, :94) | wrong-aggregation | `calib,quanta` spread: 6.866, 6.866, 4.768, **217.389**, 1.907, 1.907 (aws-20261016-223331/b: one process 224 ppm low) | rewrite | Report per-node spreads (1.9–6.9 ppm except 217.389 ppm on one c8g node), or "up to 217 ppm". |
| `clock/BENCHMARKS.md:11` | quanta held cost 15.99 / 12.50 / 10.92 with no brackets under "slowest batch in brackets" (also :44) | unsupported | `cost,quanta_now_ns` maxima 16.95 / 12.89 / 11.23 | correct | Add maxima: 15.99 (16.95), 12.50 (12.89), 10.92 (11.23). |
| `clock/BENCHMARKS.md:24` | Cost method "10,000 reads per batch, 200 batches"; calibration 20 processes | wrong-fact | c9g both nodes: `batches=50 … drift_secs=0 spawns=5`; `calib,ergon,5,…` | rewrite | State per-host settings, or mark the c9g column "50-batch / 5-process quick run". |
| `clock/BENCHMARKS.md:83` | Ordered read "3–8 ns dearer on every host; c7i's gap is the largest (7.5 ns)" | wrong-fact | now_ns gaps: c7i 7.57, c8g 9.76, c9g 0.43 | correct | Give per-host gaps (7.6 / 9.8 / 0.4 ns) and say c8g's is largest. |
| `clock/BENCHMARKS.md:84` | "gaps between consecutive reads are sub-ns on Graviton, 1–2 ns on c7i" | no-artifact | `delta,ergon_now_ns` medians 11–15 ns; Open item :100 admits the fixed gap pass has not run | delete | Delete the sentence, or write "not measured" until the fixed gap pass runs. |
| `clock/BENCHMARKS.md:14` | Calibration "40–70 ms, at most 70.226 ms" (also :52) | wrong-fact | Cited chrony runs give calib max 70.223 (aws-20261016-231320); 70.226 is aws-20261016-085442 | correct | Quote 70.223 from the cited run and name the CSV; or name 085442 if 70.226 is kept. |
| `clock/BENCHMARKS.md:94` | Stamps "must agree … Keep it on every host in the stack" | overstated | Same page :16-18 says quanta is fine within one process; no workload requirement behind "must"/"every host" | rewrite | "Where stamps are compared across processes or hosts, keep one rate on every host that exchanges stamps." |
| `clock/README.md:16` | calibrate "plus up to 50 ms for the time-sync probe" | no-artifact | 50 ms is `chrony_timeout` (clock/src/timesync.rs:969); probe duration not measured | relabel not-measured | "plus a short time-sync probe (50 ms chronyd answer timeout; duration not measured)". |
| `clock/README.md:44` | "Under about 100,000 reads a second: `std::time::Instant` is enough" | overstated | No artifact or workload requirement for the cutoff | rewrite | State the workload requirement or "not measured"; match the overview.md change. |
| `book/src/clock/overview.md:59` | 100,000 reads/sec cutoff; "needs UTC stamps … ergon-clock"; "busy-spin … ergon-clock" | overstated | No artifact for the cutoff; SystemTime/minstant also produce UTC (minstant-0.1.7 `as_nanos()`); clock/BENCHMARKS.md:11 "speed is no reason either way" | rewrite | State the workload requirement behind the cutoff (or "not measured"); scope UTC/busy-spin rows to "UTC at counter cost, or stamps compared across processes/hosts"; note quanta is equally fast in one process. |
| `book/src/clock/overview.md:135` | minstant "reads a plain `_rdtsc()`"; table "ordered read \| yes \| no \| no" | contradicted-by-code | minstant-0.1.7: `_rdtsc` is x86-only (tsc_now.rs); on Arm/macOS it is `SystemTime` (ordered wall clock) | correct | "on x86 minstant reads a plain `_rdtsc()` (unordered); on Arm and macOS it is `SystemTime`, an ordered wall-clock read". Table cell: "x86 no / Arm yes (wall clock)". |
| `book/src/clock/wall-clock.md:52,76` | Links to `#disciplined-clock-residual` and `#time-sync-in-these-runs`; "slews after boot reached 533 ppm" | unsupported | Neither anchor exists in current clock/BENCHMARKS.md (time-sync section deleted in 128747b9). 533.462 ppm is real in aws-20261016-083958/084001/085442 | correct | Retarget both links to live sections or the reference-run CSVs; keep 533.462 ppm with the run named. |
| `book/src/clock/overview.md:257` | Stall "was 7.4 / 3.2 / 3.9 µs — the VM pausing the vCPU" | overstated | Figures match `delta` maxima 7351/3153/3867 ns, but clock-summary.md marks traced gaps "unexplained" | rewrite | "the cause is unexplained in the guest; the platform's vCPU is suspected" — or "not measured". |

### deploy

| file:line | claim | verdict | evidence | disposition | exact fix |
|---|---|---|---|---|---|
| `book/src/deploy/latency-pods.md:12,145,150` | "`SCHED_FIFO`, any `sched_setscheduler`, a `--fifo` wrapper" under "Deliberately not built"; "The plugin never changes a scheduler policy" | contradicted-by-code | k8s/nri-shared-cpus builds it: `cmd/ergon-cpus/main.go:9,213` `--fifo`/`SetFifo`; `internal/wrap/wrap.go:312-326` `SCHED_FIFO`; `sched-fifo.ergon.io/` in names.go:45, grants.go:80-96; shared-cpus.md:248 documents `schedFifo: off\|thread`. Default-off and measured worse | rewrite | 1. Remove the SCHED_FIFO row from "Deliberately not built" (keep SR-IOV, netns-sysctl, IRQ). L12: "built but default-off (`ergon-cpus --fifo`, `schedFifo: thread`), measured to give no gain on an isolated CPU on AWS". 2. L150: "The NRI plugin only grants `RLIMIT_RTPRIO` and `ERGON_SCHED_FIFO`; the wrapper or the app/prelock takes `SCHED_FIFO` on the hot thread. On the default no-FIFO path PID 1 stays `SCHED_OTHER`; with `--fifo` the exec'd command runs `SCHED_FIFO`." 3. Keep shared-cpus.md:132 consistent. |
| `book/src/deploy/java.md:68` | `GuaranteedSafepointInterval=300000` "Removes the stop-the-world safepoint that otherwise runs every second" (also overview.md:51) | unsupported | aeron-samples/scripts/java-common:31 sets the flag only. JDK 27 idle 6 s: zero safepoints logged | rewrite | "raises the guaranteed-safepoint interval from the default 1 s to 300 s (Aeron's scripts set it; effect on JDK 25 not measured)". Drop the causal claim. Same at overview.md:51. |
| `book/src/deploy/java.md:91` | "A cluster's durability comes from its quorum, not from fsync" (justifies sync level 0) | overstated | aeron-archive README "Recording Durability": level 0 is page-cache only. aeron-cluster README: quorum is durability for a majority; single-node clusters exist | rewrite | "In a 3- or 5-member cluster the quorum is the durability mechanism, so sync level 0 is the usual choice; a single-node cluster's durability is fsync — raise both sync levels there." |
| `book/src/deploy/overview.md:50` | "`ParallelGC` … wins" | overstated | java.md:70 "convention, not a measured collector A/B"; :129 "**not measured**" | relabel not-measured | "so `ParallelGC` is what every Aeron script uses (convention, not a measured collector A/B)". |

### io

| file:line | claim | verdict | evidence | disposition | exact fix |
|---|---|---|---|---|---|
| `io/benches/BENCHMARK.md:101` | "stock rustls by default for TLS … not the default recommendation" (also :122) | contradicted-by-code | io/src/stream.rs:24,47: `tls: RecordLayer` required on every `TlsStream`; io/src/tls.rs:1-2 rustls handshake only, in-place decrypt. Repeats confirmed defect (book/src/io/overview.md:177) | rewrite | State the in-place `tls::RecordLayer` is the stack used (rustls handshake only); delete the "stock rustls by default" recommendation at 101 and 122. Keep decrypt-only figures as a labeled microbench. |
| `io/benches/BENCHMARK.md:151` | "ENA adaptive moderation off moved `recv-busy`'s p99.99 by about −3.0 µs (the 40 µs figure is p99.99 under load)" | wrong-fact | flags-aws-20261015T070339Z tables.md pp-recv-busy-10000: `c1-nodim 40.88 (−3.02) … 71.89 (−46.96)`, `c2-nodim 40.93 (−2.97) … 76.02 (−42.83)`. −3.0 is p50; p99.99 delta is −47/−43. Same file :50,:87,:125 say "p99.99 falls by about 40 µs" | correct | "adaptive-rx off moved `recv-busy`'s p50 by about −3.0 µs and its p99.99 by −47/−43 µs (runs `062428Z`, `070339Z`)"; delete the parenthetical. |
| `io/benches/BENCHMARK.md:56` (also :65, :99, :120, :149; `book/src/io/http-client.md:99-100`) | "HMAC-SHA256 262 ns against Ed25519 21,558 ns" as cost per request | wrong-aggregation | sign_bench.md header "ns per signed request (best of 7)" — minima, no max | relabel not-measured | Label "best of 7, max not recorded" everywhere, or re-run `sign_bench` with p50 and max. Fix http-client.md:100 nested/broken link while there. |
| `io/benches/BENCHMARK.md:122` | decrypt-only "in-place 105/175/429/1,464; stock rustls 181–2,607; OpenSSL 460–1,951" | wrong-aggregation | tls_bench.md "best of 5", no max. "181–2,607" merges rustls-unbuffered (`181.0…1669.6`) and rustls-buffered-ring-copy (`231.4…2607.1`) | rewrite | One implementation per payload as in the artifact, each "best of 5, max not recorded"; do not merge the two rustls builds. Decision cell per the TLS row above. |
| `book/src/io/http-client.md:116` | "One copy of every figure, with its max: …#4-http-clients" | wrong-fact | Anchor gone (section cut in 128747b9). Surviving BENCHMARK.md quotes p50 only, no max | rewrite | Restore the p50/p99/p99.9/max HTTP rows under a live anchor, or "p50 only; maxima live in git history" with a working link. |
| `book/src/io/rate-limits.md:185` | `try_charge` cost table (187-197), "the run is **not archived**" | no-artifact | Nothing in tree or git holds `limit_bench` results | relabel not-measured | Delete the table or mark every cell "not measured"; keep the reproduce recipe until a run is archived. |
| `book/src/io/rate-limits.md:183` | "p50 and p99 per call" = percentiles of 64-call batch means; max = slowest batch in µs | wrong-aggregation | Header "p50 ns \| p99 ns \| max batch µs" mixes per-call percentiles with a per-batch max; :211 "p50 is the number to use" | rewrite | If kept after archiving: per-call p50/p99/max in ns; per-batch max in its own table. |

### sbe

| file:line | claim | verdict | evidence | disposition | exact fix |
|---|---|---|---|---|---|
| `sbe/benchmarks/benches/perf_parity_bench.rs:16` | "Fair comparison uses `wrap` (sbe-tool's `wrap` does no bounds check)" | contradicted-by-code | ergo arm :333 runs the version-aware extent proof + panic branch inside the timer (`codegen/message_decoder.rs:346-371`); sbe-tool arm :352 is field assigns only. Violates the keep-alive rule quoted at BENCHMARKS.md:394-396 and the validation-class rule :57-64 | rewrite | Pair `wrap_unchecked` against sbe-tool's zero-check wrap (extent proven in the untimed preflight), or charge both arms the check; replace :16 with the keep-alive rule; correct the :329 "lean wrap (4 field assigns)" comment. |
| `sbe/BENCHMARKS.md:62` | Diagnostic arms `ergo-sbe_wrap_checked`, `ergo-sbe_try_from`, `ergo-sbe_header_and_body_checked`; "the gate does not read them" | wrong-fact | `ergo-sbe_wrap_checked` / `ergo-sbe_header_and_body_checked` exist only in this sentence and methodology.md:53. The gate reads `ergo-sbe_wrap`, which runs the checked constructor | rewrite | List only `ergo-sbe_try_from`; state that until the entry-point arm is swapped, the gated `ergo-sbe_wrap` runs the checked constructor. |
| `sbe/BENCHMARKS.md:422` | "The checked constructor's own cost is 21 instructions" | overstated | 21 is llvm-objdump static text size, mostly never-executed cold panic path; success path delta is `cmp`/`ja` | relabel not-measured | "static text-size delta of 21 instructions (mostly never-executed cold panic path)"; executed cost "not measured" or cite Callgrind Ir/op. |
| `sbe/BENCHMARKS.md:497` | equal 80/80 counts prove "the arms retire the same work" and exonerate the wall-clock gap | unsupported | Static text sizes only; line 365-367 "Differing instruction texts are 14 per pair"; Callgrind lane still needed | relabel not-measured | State only that static text sizes match across profiles; drop the exoneration, or write "retired work not measured". |
| `sbe/BENCHMARKS.md:582` | "Packing does **not** beat the wire-image design … ~60x slower than any single-field access" | overstated | Unprovenanced run: one composite, one field, test buffer | rewrite | Scope to the measured case ("in this unprovenanced run, for the f15 field, …") or "not measured". |
| `sbe/benchmarks/tests/bench_gate_test.rs:372` | overrides "stay within 1.01" | wrong-fact | Test enforces `PROFILE_SCOPED_OVERRIDE_MAX` with `session_event` → 1.05 (`scripts/check-bench-gate.sh`) | correct | "stay within its documented maximum in `PROFILE_SCOPED_OVERRIDE_MAX`". Do not change any ceiling. |

### cluster-md

| file:line | claim | verdict | evidence | disposition | exact fix |
|---|---|---|---|---|---|
| `book/src/md-dpdk/overview.md:243` (also :20-23, :82, :159-161) | Localhost row `5.48 / 8.18 / 10.73` vs `5.58 / 7.39 / 8.86`; glossary "max = the slowest single round trip" | wrong-aggregation | aws-20261015-012400 rtt isolated:local-dpdk per-rep maxima 10727, 664575, 11159, 9983, 9967 ns. Printed 10.73 is the median of those; absolute worst 664.58 µs. kernel 8.86 vs 9.46 (small) | rewrite | Print "max 10.73 (worst rep 664.6)" or the absolute worst; reword :82 and :159-161 so "max" is labeled median-of-rep-maxima and the worst rep is named; fix the banner :20. |
| `md-dpdk/README.md:23` (also :23-29) | "p50 / p99.99 / max: 5.48 / 8.18 / 10.73 µs against … 5.58 / 7.39 / 8.86 µs"; Graviton "max higher (19 against 15)" | wrong-aggregation | Same rows; Graviton aws-20261016-072005 maxima 18.66/35.07/12.30/19.86/16.86 vs 20.25/12.81/15.03/13.83/15.29. No method note; README drops the range BENCHMARKS.md prints | rewrite | Quote absolute maxima (664.6 / 9.5 µs) or label "median of 5 rep maxima" with the worst rep beside it; add the one-line method. |
| `book/src/md-dpdk/overview.md:222` (also :220-221, :201-203, :179-181) | Between-hosts max columns (e.g. Azure DPDK "20.11 / 30.29 / 71", kernel-busy "101.50 / 21,152") under "max is the slowest one" | wrong-aggregation | Printed medians of 5 rep maxima hide the worst: 71 hides 447.5; 289 hides 2,740; 82 hides 449; 21,152 hides 33,325; 2,574 hides 13,943; 599 hides 6,664; G1 75 hides 154.9, 107 hides 166.1 | rewrite | Give each max as the absolute worst, or median (worst), matching BENCHMARKS.md's "max 71 (48–845)"; never define the column as "the slowest one" while printing a median. |
| `book/src/md-dpdk/overview.md:149` | busy-poll ring "under load its max reached 0.6 ms on c7i and 21 ms on Azure" | wrong-aggregation | 0.6/21 ms are medians of rep maxima; peaks reached 6.7 ms (aws-20261015-185813) and 33.3 ms (20261014-173745) | correct | "its max reached 6.7 ms on c7i and 33 ms on Azure (median of rep maxima 0.6 and 21 ms)". |
| `md-dpdk/README.md:153` | "two 288 B flows 5,070 NAKs → none" | no-artifact | 5,070 appears nowhere in BENCHMARKS.md, summaries or results. aws-20261015-174836/isolated/run.log: two-flow points discarded ("windows overlapped 0.001 … analyse.py leaves the run out") | delete | Delete the clause, or "two-flow effect not measured (points discarded: overlapping windows)". Keep 400 k → 600 k. |
| `md-dpdk/BENCHMARKS.md:27` | "the 8 cores of a c7i.4xlarge pair ($1.428)" | wrong-fact | c7i.4xlarge is 8 cores × 2 with HT = 16 physical per pair (overview.md:195); c9g.2xlarge is 8, no SMT (overview.md:173) | correct | "the 8 cores of a c7i.4xlarge (16 per pair) at half the price" — name cores per host or the pair's 16. |

### rest

| file:line | claim | verdict | evidence | disposition | exact fix |
|---|---|---|---|---|---|
| `README.md:101` (also `CONTRIBUTING.md:64`, book include) | PR CI "executes 32-bit x86 plus big-endian s390x codec tests" through cross/QEMU | no-artifact | No such step in any workflow. Only BE-adjacent lane is weekly Miri on sbe/miri-fixtures (nightly.yml:48) | delete | Delete the clause in README.md and CONTRIBUTING.md. Optionally: "not run in CI yet; LE/BE fixture codecs are plain-tested on PR CI and Miri-tested weekly". |
| `README.md:102` (also `CONTRIBUTING.md:60-61`) | "critical-path mutation testing weekly" | contradicted-by-code | justfile:410-413 "MANUAL gate — too slow for CI"; no workflow runs `cargo mutants`; only `check-mutation-config.sh` | rewrite | "mutation testing is manual (`just check-mutation`); CI enforces only the mutation config (`check-mutation-config.sh`)". |
| `README.md:102` (also `CONTRIBUTING.md:62`) | "Miri fixtures nightly" | wrong-fact | nightly.yml cron `"23 17 * * 0"` — weekly Sunday | correct | "weekly (Sundays)" in both files (or change the cron). |
| `book/src/project/road-to-1.0.md:86` | semver-checks "against the 0.1.21 baseline" | wrong-fact | api/public-api-baseline.toml:63 `baseline_tag = "v0.1.26"` | correct | "the `baseline_tag` baseline in `api/public-api-baseline.toml` (currently 0.1.26)". |
| `book/src/tune/overview.md:3,129` | "Each finding says … what it cost in this repository's measurements"; "One A/B per check" | overstated | Same page :135-137: T4, T5, T6, T9, N2, N3 not measured | rewrite | "where measured (unmeasured checks say so and cite the vendor's documentation)"; "One A/B per measured check". |
| `book/src/samples/build-patterns.md:9` | `src/generated/` pattern is "(product / samples default)"; OUT_DIR "fine for apps; poor IDE go-to-def" | contradicted-by-code | cluster/build.rs:36 uses `generate_to_out_dir`; introduction.md:9, README.md:18 document OUT_DIR + `sbe_mod!`. Only unpublished samples write `src/generated/` | rewrite | Label `src/generated/` "samples default (unpublished, gitignored)"; OUT_DIR + `sbe_mod!` is the product/default pattern. |
| `book/src/samples/overview.md:12` (also `samples/README.md:12`) | L3 Order Book "build-dep only (plain `include!`)" | contradicted-by-code | samples/l3-book/src/lib.rs:21 is `#[path = "generated/l3_codec.rs"]` | correct | "build-dep only (`#[path = "generated/…"]`, not `include!`)" in both files. |
| `book/src/project/package-scope.md:18` | "Cluster codecs stay crate-private" | contradicted-by-code | cluster/src/lib.rs:106-111 `#[doc(hidden)] pub mod cluster_codec_types` | correct | "not a stable consumer API (`#[doc(hidden)] pub mod cluster_codec_types`, tests and benches only); use `AeronCluster`". |
| `book/src/samples/buffer-sizing.md:3` | "Every protocol buffer must be sized using the generated EncodedLength API. Never guess with `vec![0u8; 4096]`" | overstated | Generated MAX_ENCODED_LENGTH doc sanctions a true fixed bound stack array; :26 of the same page is the defensible wording | rewrite | "Size buffers with the generated length APIs; a `MAX_ENCODED_LENGTH` stack array is fine when that constant is a true fixed bound. Avoid guess constants like 4096." |

### Prior confirmed defects still needing cleanup

| file:line | claim | verdict | evidence | disposition | exact fix |
|---|---|---|---|---|---|
| `book/src/deploy/benchmarks.md:28,37` (metal row) | "max is the slowest single round trip"; metal DPDK idle max 71 µs | wrong-aggregation | aws-20261015-012400 cross-host per-rep maxima; worst repetition 845311 ns; 71 is the median of 5 rep maxima | rewrite | Report the absolute worst (or "median (worst)"); reword the method prose so "max" is not defined as the slowest single round trip. |
| `io/benches/src/stats.rs` (`Work::from_meta`) | counts accepted without `Work::check()` | contradicted-by-code | Contradictory counts (scheduled 1, issued 8, completed 7, censored 5) accepted | correct | Call `Work::check()` in `from_meta` (or at the load boundary) and report malformed meta instead of accepting it. |
| `samples/l3-book/tests/l3_tests.rs` (`best_bid_and_depth`) | Decimal equality of prices | wrong-fact | Compares mantissas, ignores exponents | correct | Compare `rust_decimal::Decimal` values (mantissa and scale), or normalise both sides before asserting. |
| `scripts/book-fence-allowlist.txt` | three fence allowlist entries at 186, 194, 233 | wrong-fact | Live fences are at 188, 196, 235 (`book/src/tune/overview.md`) | correct | Update the three line numbers to the live fence lines. |

## 2. Stages

### (1) Restore trustworthy evidence
Wrong aggregation, no artifact, mixed runs.

- clock: `clock/BENCHMARKS.md:58`, `:64`, `:13`, `:52`, `:12`, `:11`, `:24`, `:83`, `:84`, `:14`
- io: `io/benches/BENCHMARK.md:56` (+ repeats), `:122`, `:151`; `book/src/io/http-client.md:116`; `book/src/io/rate-limits.md:185`, `:183`
- cluster-md: `book/src/md-dpdk/overview.md:243`, `:222`, `:149`; `md-dpdk/README.md:23`, `:153`; `book/src/deploy/benchmarks.md:28,37`
- rest: none

### (2) Repair benchmark validity
Accounting, completeness, barriers, ceilings preserved.

- `sbe/benchmarks/benches/perf_parity_bench.rs:16` (unequal validation work inside the timer)
- `sbe/BENCHMARKS.md:62` (nonexistent diagnostic arms; gate runs the checked constructor)
- `sbe/BENCHMARKS.md:422`, `:497` (static text size sold as executed cost / retired work)
- `sbe/benchmarks/tests/bench_gate_test.rs:372` (comment says 1.01; ceilings untouched)
- `io/benches/src/stats.rs` (`Work::from_meta` accounting)
- Preserve: `PROFILE_SCOPED_OVERRIDE_MAX` ceilings, `NOISE_FLOOR_CEILING_EXCEPTIONS`, bench-gate ratios ≤ 1.00

### (3) Simplify components
Clock recommendation, TLS custom record layer, deployment machinery, NRI/DPDK optional.

- Clock recommendation: `book/src/clock/overview.md:59`, `:135`; `clock/README.md:44`; `clock/BENCHMARKS.md:94`
- TLS record layer: `io/benches/BENCHMARK.md:101`, `:122` (and `book/src/io/overview.md:177`, already confirmed)
- Deployment machinery: `book/src/deploy/java.md:68`, `:91`; `book/src/deploy/overview.md:50`
- NRI/DPDK optional: `book/src/deploy/latency-pods.md:12,145,150` (SCHED_FIFO is built, default-off)

### (4) Correct examples and rewrite the book
l3 price compare, timestamps, DTOs, OUT_DIR, links, categorical claims.

- l3 price compare: `samples/l3-book/tests/l3_tests.rs`
- Timestamps: `book/src/clock/wall-clock.md:52,76` (anchors, 533 ppm provenance); `book/src/clock/overview.md:257` (stall attribution)
- DTOs / generated-code examples: `book/src/project/package-scope.md:18`; `book/src/samples/buffer-sizing.md:3`
- OUT_DIR / codegen patterns: `book/src/samples/build-patterns.md:9`; `book/src/samples/overview.md:12`; `samples/README.md:12`
- Links: `book/src/io/http-client.md:116`; `book/src/clock/wall-clock.md:52,76`; `book/src/project/road-to-1.0.md:86`
- Categorical claims: `README.md:101`, `:102`; `CONTRIBUTING.md:60-64`; `book/src/tune/overview.md:3,129`; `sbe/BENCHMARKS.md:582`
- Allowlist: `scripts/book-fence-allowlist.txt`

## 3. Delete outright (do not correct)

| claim | file:line | reason |
|---|---|---|
| "gaps between consecutive reads are sub-ns on Graviton, 1–2 ns on c7i" | `clock/BENCHMARKS.md:84` | No artifact; fixed gap pass not run. Corrupting to a number would invent one. |
| "two 288 B flows 5,070 NAKs → none" | `md-dpdk/README.md:153` | No artifact; the two-flow points were discarded by the analysis. Nothing to correct to. |
| `rate-limits` cost table cells | `book/src/io/rate-limits.md:185-197` | "the run is not archived" — every cell is unpublished. Keep the reproduce recipe; delete or mark "not measured". |
| "executes 32-bit x86 plus big-endian s390x codec tests" | `README.md:101`, `CONTRIBUTING.md:64` | The lane does not exist. Relabeling as "not implemented" is acceptable only if the actual coverage sentence replaces it. |
| "stock rustls by default" recommendation | `io/benches/BENCHMARK.md:101`, `:122`; `book/src/io/overview.md:177` | The stack does not have a stock-rustls record path. The recommendation is not a mis-measurement; it describes software that is not there. |
| "rate vs kernel scaling" column | `clock/BENCHMARKS.md:52` | Duplicate of the drift column with different numbers; one measurement cannot be two columns. |
| "(the 40 µs figure is p99.99 under load)" | `io/benches/BENCHMARK.md:151` | A delta is not a level; the parenthetical misdefines the figure. |
| `ergo-sbe_wrap_checked`, `ergo-sbe_header_and_body_checked` as named arms | `sbe/BENCHMARKS.md:62`; `sbe/benchmarks/methodology.md:53` | No bench registers these labels. Listing them invents arms. |
| c9g discipline cells `310 / 1,610 (1,610–2,400)` and c8g `2,100` | `clock/BENCHMARKS.md:64` | No artifact row contains these as residual statistics. Replace with artifact values (preferred) or delete. |
| Broken anchors `#4-http-clients`, `#disciplined-clock-residual`, `#time-sync-in-these-runs` | `book/src/io/http-client.md:116`; `book/src/clock/wall-clock.md:52,76` | Targets no longer exist. Delete the link text or point at a live section; do not leave a dead "one copy of every figure, with its max" promise. |

## 4. Defects the inventory missed

| file:line | claim | verdict | evidence | fix |
|---|---|---|---|---|
| `samples/README.md:42` | OUT_DIR "fine for apps; **poor IDE go-to-def**"; `src/generated/` implied default | contradicted-by-code | Same text as `book/src/samples/build-patterns.md:9`, which the inventory covers; this copy is not in any row | Same fix as build-patterns.md:9 (product default is OUT_DIR + `sbe_mod!`). |
| `book/src/deploy/benchmarks.md:37` | Table header "idle … max \| 1 M msgs/s … max" with method "max is the slowest single round trip" | wrong-aggregation | Confirmed defect covered only the metal 71 µs cell. Every max cell in this table is a median of 5 rep maxima under the same absolute definition | Apply the overview.md:222 fix to all rows of this table, not just the metal cell. |
| `io/benches/BENCHMARK.md:56,62,116,149` | HTTP client latency "10–11 µs against libcurl's 28–30 (p50)" | quantitative | p50 only, no max, against the file's own rule at :3 ("every latency figure keeps its max"). Pre-cut git history had the maxima (e.g. libcurl `79.7 / 95.7 / 126.6 / 269`) | Restore the maxima beside the p50s, or label "p50 only; maxima in git history". |
| `book/src/md-dpdk/overview.md:237-238` | Localhost method prose | wrong-aggregation | Same "median of 5 reps / max is the slowest" contradiction as :159-161; noted as adjacent in verification, never given a row | Same caveat as the :243 fix. |
| `sbe/BENCHMARKS.md:160` | Gate table row lists `wrap(…, 8, …)` for the entry-point pair | wrong-fact | Contradicts the same validation-class rule that the perf_parity finding enforces; the bare `wrap` runs the extent proof | Update the row once `perf_parity_bench.rs:16` is fixed. |
| `CONTRIBUTING.md:60-64` | "Nightly CI runs every fuzz target …" | wrong-fact | `nightly.yml` is weekly (`cron: "23 17 * * 0"`); the workflow name lies | Change "Nightly CI" to "Weekly CI (Sundays)" wherever it appears. |
| `book/src/io/kubernetes.md:92-93`; `book/src/io/overview.md:133,260` | "a little at p50 … more at p99"; "lowest p99 of the polling backends from about 10 sockets" | quantitative | Directional p50/p99 claims with no max, no run id, no numbers | Give the figures with their max and the run, or delete the comparative claim. |