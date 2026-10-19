# Benchmarks

> **Benchmark review requested.** Generated-codec benchmarking is notoriously
> difficult and easy to get wrong. Surprising results should be presumed to be
> benchmark defects until wire parity, equal work, optimizer opacity,
> sufficiently amplified timing, both LTO profiles, and optimized
> assembly/instruction counts agree. Please review the methodology and report
> mistakes; these tables are evidence under review, not unquestionable facts.

ergon's maintained benchmarks compare generated codecs with official
**sbe-tool** output performing equivalent work. This page is the single home
for the method, the gate, the measured results, and how to run the suite.
A benchmark number is only evidence for the tree that produced it: the result
of record is the artifact `just bench` writes, never a table copied into prose.

Quote a result by naming its run id, commit, host, rustc, profile, and manifest
hash — or do not quote it.

## Where the current result lives

Each `just bench` invocation owns one unique result root under
`target/bench-runs/<run-id>/<profile>/criterion`, containing:

- `run-manifest.json` — run id, profile, commit, rustc, target;
- one Criterion directory per benchmarked arm, each stamped with that run id.

The gate reads only that root. It fails closed on missing, incomplete, stale, or
mixed-run results, so a release can never pass on evidence from another run.
Both profiles (LTO and no-LTO) are blocking.

## Recommended consumer build profile — enable LTO

Build against ergo-sbe with link-time optimization on:

```toml
[profile.release]
lto = true
codegen-units = 1
```

LTO gives the optimizer more scope across crate boundaries. Public generated
hot-path methods also carry `#[inline]` so downstream crates can optimize calls
without LTO. The effect depends on the schema, access pattern, compiler, and
surrounding application; this profile does not by itself establish parity or a
speed advantage. Measure both profiles. Both remain required by the maintained
gate. Missing `#[inline]` is the defect the no-LTO profile exists to catch.

## What the numbers actually measure

Most of the measured difference between ergo-sbe and sbe-tool comes down to
**bounds checking**, not fundamental codegen quality. sbe-tool's `wrap` +
`header()` performs no bounds, header, or version check. Ergon's `wrap` and
`wrap_and_apply_header` do: they prove the version-aware fixed extent before
handing back an encoder or decoder whose accessors are unchecked. Timing those
against sbe-tool would charge ergon for work its reference never does.

Every maintained pair is therefore in one **validation class**, currently
`none` for all of them: the extent proof runs once in an untimed preflight
(`assert_baseline_wrap_extent`, `assert_stream_wrap_extent`,
`assert_encode_extent`), and the timed region calls the matching unchecked
constructor. `fairness_policy_test` enforces this mechanically. Checked
constructors are still benchmarked as separately labelled diagnostic arms
(`ergo-sbe_wrap_checked`, `ergo-sbe_try_from`,
`ergo-sbe_header_and_body_checked`); the gate does not read them.

Dynamic tails still validate their dimensions and lengths while being
consumed. The staged decoder obtains each dynamic entry's end from the
callback's completion, so it need not scan that entry before visiting it.
Calling `verify()` before decoding, or mixing random-access tail lookahead with
a subsequent consuming walk, would add traversal work.

The maintained Car `ergo-sbe_ordered` arm walks with recursive ordered
callbacks at every tail and observes the same fields as the staged consuming
arm (no extra `EntryInfo` index). Lane-to-lane cost is `versioned_l3_bench`,
not the `1.00` sbe-tool gate.

The gate exists to prove that ergo-sbe is **not slower than** sbe-tool — not
to claim it is faster. When ergo-sbe shows a lower ratio, investigate for
unequal work; do not treat it as a product claim.

The real win is elsewhere: ergo-sbe pushes wire-order safety and buffer sizing
into the **type system at compile time**. Swapping `bids` and `asks` is a type
error. Using a partially-written encoder as a complete message is a type error.
Allocating a scratch buffer with a guessed size instead of using the exact
`EncodedLength` is a compile-time lint.

## Fairness rules

Every parity benchmark must compare identical logical work. The mechanical
guard is `cargo test -p ergo-sbe-benchmarks --test fairness_policy_test`; it
supplements the runtime byte/value assertions and does not prove equal work by
itself.

### Header work: match sbe-tool — both write it, or both skip it — never mix

Ergon must **not** always call `wrap_and_apply_header`. Mirror the sbe-tool
arm. Official sbe-tool Rust full-wire order
(`simple-binary-encoding/rust/benches/car_benchmark.rs`):

```text
enc = enc.wrap(buf, ENCODED_LENGTH);     // body starts at 8
enc = enc.header(0).parent()?;           // write MessageHeader at 0, then body
// body setters…
let body_len = enc.encoded_length();     // body only — NOT header-inclusive
```

| Mode | ergon | sbe-tool |
|---|---|---|
| **Body only** (cluster encode gates) | `wrap(buf, 0)` + setters | `wrap(buf, 8)` + setters, **no** `.header(0)` |
| **Header + body** | `wrap_and_apply_header` + setters | `wrap(8)` then `header(0).parent()` **before** setters |
| **Header only** | `wrap_and_apply_header` alone | `wrap(8).header(0)` alone |

**Length traps:** sbe-tool `encoded_length()` is body-only. Do **not** assert
`8 + encoded_length()` as if that proved a header write when `.header(0)` was
never called. Body-only pairs leave `[0..8)` zero on both arms and compare body
bytes only. Invalid: ergon `wrap_and_apply_header` vs sbe-tool body-only
`wrap(…, 8)`. Decode: both direct decoders take absolute body offset
(`message_offset + 8`).

Tiny encode comparisons publish three cases — header only, body only, header
and body — so ergon's single template copy versus sbe-tool's four header
setters is not misreported as scalar-field performance.

### Correctness and equal work

- Encode comparisons assert identical lengths and bytes before `b.iter`.
- Decode comparisons assert identical fixed values, group entries, nested
  entries, and var-data before `b.iter`.
- Assertions never run inside the timed path.
- Read or write the same fields the same number of times.
- Either both arms parse a header or neither does; either both construct
  wrappers inside the timer or both precompute them.
- Traverse every corresponding group entry and var-data field.
- Keep allocation, input construction, sizing, and fixture replication outside
  the timed path. Reuse equally sized caller-owned buffers.

### Optimizer opacity

Use `std::hint::black_box`, not Criterion 0.5's fallback implementation. Make
source values, input slices, or prebuilt decoder references opaque before the
measured access. Observe the encoded byte range after writes.
`std::hint::black_box(&mut buf)` makes the mutable reference opaque, but the
suite also observes the written range so it does not rely on reference escape
alone. Never gate a single sub-nanosecond operation: repeat identical logical
work inside each Criterion iteration and declare the repeated element count as
throughput, or harness and code-placement effects can reverse the apparent
winner.

### Maintained pair modes

Every gated ergon/sbe-tool pair uses the same header mode on both arms:

| Gate | Mode | ergon | sbe-tool |
|------|------|-------|----------|
| encode/scalar header+body | full wire | `wrap_and_apply_header` + 2 fields | `wrap(8)` + `header(0).parent()` + 2 fields |
| encode/scalar body only | body only | `wrap(0)` + 2 fields | `wrap(8)` + 2 fields, no header |
| encode/throughput 10k | full wire | apply-header + 2 fields | wrap+header+parent + 2 fields |
| wire_parity encode full | full wire | apply-header + full Car | wrap+header+parent + full Car |
| decode scalar/array/composite | accessors only | prebuilt decoder | prebuilt decoder |
| decode entry wrap | body wrap | `wrap(…, 8, …)` | body decoder at `msg+8` |
| decode full / batch 10k | body wrap + same fields | same | same |
| cluster encode (all 3+claim) | **body only** | `wrap(0)` + fields | `wrap(8)` + fields, no header |
| cluster decode | header+body parse | `wrap_and_apply_header` | MessageHeaderDecoder + body + equal checks |

`header+body` pairs ergon's capacity-checked `wrap_and_apply_header` against
sbe-tool's `parent()` Result. `body only` pairs the unchecked body path against
sbe-tool's unchecked `wrap`. Making the column uniform would break the pairing.

Scalar body encoding uses 1,024 successive fixed-block destinations in one
preallocated slab; both arms write the same two fields. Inputs become opaque
once per batch; a compiler memory clobber preserves every destination's stores.
Reusing one destination is a useful diagnostic, but its LTO hot loops reduce to
identical instructions and does not model encoding successive outbound
destinations. This body-only case omits the header write and dynamic tails.

Diagnostics (encode_style, encode_bench, l2_book, group_decimal DTO arms,
throughput/checked) are ergon-only or DTO-vs-DTO — not ergon/sbe-tool ratios.

## The gate

Ratios are ergon / sbe-tool. The project target is **`1.00`** for every
maintained comparison. `scripts/check-bench-gate.sh` applies that ceiling with
zero tolerance beyond each comparison's profile-specific exception. `just
bench` and `just bench-cluster` pass `--run-id` so stale Criterion trees cannot
pass. Check actual ratios as well as the pass/fail result: a gate pass is not
literal `1.00` everywhere.

| Comparison | no-LTO | LTO |
|---|---:|---:|
| Cluster session-message-header decode | 1.01 | 1.01 |
| Cluster session-event decode | 1.05 | 1.01 |
| All other maintained comparisons, including SBE `optional_enum_nullify` and every cluster encode | 1.00 | 1.00 |

A ceiling above `1.00` records a repeatable, fair sbe-tool win; it is not
permission to add overhead. Changing a ceiling requires a fresh fairness audit
and recorded measurements. The cluster allowances are not proof of literal
parity.

Both profiles are blocking. The no-LTO profile
(`CARGO_PROFILE_BENCH_LTO=false CARGO_PROFILE_BENCH_CODEGEN_UNITS=1`) catches
missing `#[inline]` on generated hot paths, which LTO hides. It is also the
tightest: without cross-unit inlining, ergon's margin narrows, and scenarios
with the least work to hide sit close to 1.00×. Ratios there are sensitive to
thermal and code-layout variance — re-run on an idle machine before
investigating a single high ratio, and check Criterion's confidence intervals
for overlap. The LTO profile is the one consumers should build with. Neither
profile is a soft warning.

The gate uses Criterion's regression estimate consistently (a previous revision
mixed it with the raw sample median and could reverse a tiny ratio). It fails
if any expected estimate is missing; a renamed or removed maintained benchmark
cannot silently pass as “skipped.”

### Cluster codec gate

The Cluster suite applies the same equal-work rules. Encode gates time
**body-only** field writes on both arms. Connection, authentication, and
leader-change operations are cold-path diagnostics unless a recipe marks them
as gates. Five maintained scenarios: encode/session_message_header,
encode/session_keep_alive, decode/session_message_header, decode/session_event,
encode/claim_shaped_header_plus_app. Cluster encode arms locally assert exact
sbe-tool byte parity before timing and reuse one pre-sized buffer per function.
Decode arms locally assert the same scalar, enum, and var-data values before
timing.

## Measured results

Conclusions and the numbers this repository keeps in prose. Unprovenanced
diagnostics (no run-id manifest) are labelled as such: reproduce them and read
the live numbers rather than treating a quoted figure as current.

### Cluster codec ratios (gated suite)

All 5 maintained scenarios passed at the time of recording:

| Scenario | Ratio | Status |
|----------|-------|--------|
| encode/session_message_header | 0.7464 | PASS |
| encode/session_keep_alive | 0.6213 | PASS |
| decode/session_message_header | 0.8968 | PASS |
| decode/session_event | 0.8209 | PASS |
| encode/claim_shaped_header_plus_app | 0.8288 | PASS |

### Root cause of prior cluster encode regression (FIXED)

History: the two cluster encode scenarios (`session_keep_alive`,
`claim_shaped`) once failed at 1.19× and 1.28× (before-figures from the
original report; the runs were not archived). Generated field setters used
`self.buf[offset..offset+N].copy_from_slice(...)`, which re-checks bounds on
every field write. After `wrap`/`wrap_and_apply_header` validates
`buf.len() >= BLOCK_LENGTH`, field offsets are in-bounds by construction.

**Fix:** field setters now use `get_unchecked_mut` after the trust boundary.
Both are sub-1.00 today — `encode/session_keep_alive` and
`encode/claim_shaped_header_plus_app` in the [gate table](#the-gate), which
is the artefact; re-run `just bench` for the current pair.

### Group encode and DTO bulk

sbe-tool performs consistently with and without LTO because its generated hot
methods carry explicit inline intent; ergon's group entry setters now carry the
same intent (fixed after a prior defect where LTO-off ergon lost to sbe-tool
for want of `#[inline]`; 2026-07-29 audit).

`just bench-groups` compares `bulk_add`, `add_struct`, and `add_closure`
against sbe-tool for primitive and `Decimal`-composite entries in both
profiles. On the audited 1,000-entry Apple M4 run, automatic DTO bulk measured
509.1 ns versus 1.336 µs with LTO and 508.6 ns versus 1.998 µs without LTO.
The `dto_add_reference` / `dto_auto_bulk` arms are a DTO-to-DTO diagnostic:
both perform checked buffer entry and the same three writes per entry, with DTO
construction outside timing. The automatic arm is not a direct sbe-tool ratio
because sbe-tool has no equivalent owned DTO/range-validation API. This
diagnostic is unprovenanced; reproduce with `just bench-groups`.

### Self-comparison: catch regressions against your own previous release

The sbe-tool ceiling does **not** catch regressions against your own prior
work. If ergon was 0.73× sbe-tool in 0.1.7 and 0.89× in 0.1.8, both pass the
1.00 ceiling — but you just got 22% slower. That is a blocking defect.

Every release must compare **two** things:

1. **Ratio vs sbe-tool** — must stay ≤ 1.00.
2. **Absolute ergon time vs the previous release** — check out the prior tag
   in a worktree, run the same benchmarks, and diff the Criterion point
   estimates. A shift larger than the reported confidence interval requires
   investigation before publishing.

The second check found the `msg_offset` regression in 0.1.8:
`decode_entry_point` went from 0.73× to 0.89×. The sbe-tool ratio still
passed — only the self-comparison caught it.

### Layout access (diagnostic) — `layout_access_bench`

Not a ≤1.00 gate. Compares flyweight vs wire-image value vs
`#[repr(C, packed)]` for a single mid-block field on a 256-byte composite
(`BigBlock`, field `f15`). **Conclusion:** single-field access is one load for
flyweight, preheld `[u8; N]` wire image, and packed overlay alike. Packing does
**not** beat the wire-image design. Materialising the whole composite first
(`value_copy_then_f15`) is ~60x slower than any single-field access — the
expensive path is the copy, not the read. Unprovenanced. See
[Composite layout & little-endian](https://mimran1980.github.io/ergon/sbe/core-concepts/composite-layout.html).

```sh
cd sbe/benchmarks && cargo bench --bench layout_access_bench
```

### Encode style (diagnostic) — `encode_style_bench`

Not a ≤1.00 gate. **Conclusion:** `setters_all_fixed` and `fixed_struct`
(`.fixed`) measure equivalently — the staged builder is not a tax over
individual setters. For the 256-byte composite block, LE and BE build paths
are close, with BE slightly slower on an LE host; preheld-memcpy arms are
endian-independent and slower than either build path. Unprovenanced.

```sh
cd sbe/benchmarks && cargo bench --bench encode_style_bench
```

### Alignment experiment

`alignment_bench` exercises message offsets `0..=63` for stack arrays, reused
`Vec` storage, and a `#[repr(align(64))]` test buffer. Across every offset,
all three measure equivalently — the aligned buffer did not improve the
aggregate result, so this release adds no mandatory aligned-buffer or pooling
API. SBE frames remain valid at arbitrary caller-selected offsets.
Unprovenanced.

```sh
cargo bench -p ergo-sbe-benchmarks --bench alignment_bench
```

### u32 compact tail-offset (removed)

A `u32` compact tail-offset representation was tried and **removed**:
smaller structs, more instructions, slower under LTO. Cached tail ends stay
absolute `usize` (see `tail_end` in `sbe/src/codegen/`). The measurement
that decided it was not archived, so no figures are quoted here — re-derive
it from `just bench` if the question comes back.

## Diagnostics (not gated)

| Command | What it answers |
|---------|-----------------|
| `just bench-groups` | `bulk_add` / `add_struct` / `add_closure` vs sbe-tool |
| `layout_access_bench` | flyweight vs wire-image vs `repr(C, packed)` |
| `encode_style_bench` | `.fixed` vs setters; LE vs BE body build |
| `just bench-diagnostics` | size/count/var-data/endian/version matrix; `versioned_l3_bench` lane trade-off |
| `alignment_bench` | offsets `0..=63` |
| `instruction_counts` | amplified wall-clock of tiny ops (not Ir counts) |
| `just bench-instructions` | Callgrind Ir/op; fails if ergon exceeds sbe-tool |
| `latency_distribution` | warmed p50/p99/p99.9 for 1,000-message batches |
| `just bench-cold` | schema parse / codegen / artifact size |

None of these stamp a gated run-id except `just bench` / `just bench-cluster`.
Read live Criterion output.

`just bench-diagnostics` runs `codec_matrix_bench` over fixed blocks 16/64/256
bytes, group counts 0/1/5/20/100, var-data 0–8192 bytes, flat and ragged
nested shapes, LE/BE/custom header, acting versions 0 and 1, and the access /
encode / sizing / DTO / round-trip operations. It never uses IronSBE, rustysbe,
handwritten offsets, or a custom wire format as an oracle. Unprovenanced.

`instruction_counts` is an amplified Criterion **timing** harness (wall-clock
estimates, not instruction counts). `latency_distribution` reports p50, p99,
p99.9 for warmed 1,000-message batches; the tool prints those three only — any
latency figure quoted in prose must also carry its max.

Instruction and disassembly evidence comes from `perf-probe` and
`scripts/run-sbe-instruction-probes.sh`, not Criterion:

```sh
./scripts/run-sbe-instruction-probes.sh --all-profiles      # or `just bench-instructions`
./scripts/run-sbe-instruction-probes.sh --profile lto --probe ergo_probe_decode_composite
```

Each probe is a named, `#[inline(never)]` wrapper doing 10,000 opaque logical
operations. The driver builds distinct LTO and no-LTO artifacts, runs Valgrind
Callgrind (`--toggle-collect`), normalises Ir/branch/mispredict **per
operation**, and disassembles with `llvm-objdump`. The registry in
`sbe/benchmarks/src/bin/perf_probe.rs` (checked-in copy `probes.tsv`) is the
manifest; unknown or drifted probe names fail closed. The lane needs Linux +
Valgrind + `llvm-objdump` and **fails closed** elsewhere. There is deliberately
no `iai-callgrind` dependency — removed for RUSTSEC-2026-0173 and must not
return. On Linux the lane fails when any registered two-arm pair has ergon
Callgrind Ir/op above sbe-tool. `just bench` still gates wall-clock only.

## Regression policy

- Local audits and dedicated stable runners keep the sbe-tool equal-work gate
  with a `1.00` ceiling for every maintained comparison under LTO and no LTO.
- Shared GitHub CI runs both profiles and publishes Criterion diagnostics, but
  does not use noisy wall-clock ratios as a merge gate.
- A dedicated stable runner, when configured, must reject a hot-path Criterion
  regression-estimate increase above 3%, a normalised instruction-count
  regression above 2% from `scripts/run-sbe-instruction-probes.sh`, any new
  allocation, or a warmed batch/cluster p99 regression above 5%.
- Criterion's regression estimate and confidence interval are the maintained
  microbenchmark estimator. HDR p50/p99/p99.9 applies only to warmed batch and
  Aeron/cluster end-to-end measurements.

The expanded Criterion matrix, alignment, cold-path, and HDR suites were
executed on 2026-07-27 with rustc 1.95.0. The instruction-probe lane needs
Linux and Valgrind, so it does not run on a macOS development host — it fails
closed there. Machine-specific observations belong in CI artifacts or a
release record; they are not portable API promises.

## How to run

```sh
just bench                 # SBE codec parity suite and ratio gate
just bench-cluster         # cluster codec gate
just bench-groups          # group / Decimal group encode, both LTO profiles
just bench-diagnostics     # expanded non-gating diagnostics
just bench-instructions    # Linux + Valgrind Callgrind; fails closed elsewhere
just bench-cold            # schema parse / codegen / artifact size
```

Workspace benchmark profile is LTO=true, codegen-units=1. To reproduce a
no-LTO run in isolation:

```sh
CARGO_TARGET_DIR=target/bench-no-lto \
CARGO_PROFILE_BENCH_LTO=false \
CARGO_PROFILE_BENCH_CODEGEN_UNITS=1 \
cargo bench -p ergo-sbe-benchmarks --bench group_encode_bench
```

Run the strict gate locally or on a dedicated stable runner. Shared GitHub
runners execute both suites and publish diagnostics, but do not turn noisy
wall-clock ratios into merge decisions.

## Interpreting results

Criterion reports live under `target/criterion/`. Review the regression
estimate and confidence interval, not a single noisy iteration or a different
estimator selected after seeing the result. For a material generator change:

1. run on an otherwise idle machine;
2. record the commit, Rust toolchain, target, profile, and host;
3. confirm both arms execute the intended body;
4. repeat suspicious or borderline comparisons;
5. keep the change only if every maintained ratio stays within its reviewed
   ceiling.

Capture immutable numbers in a release artifact when a release needs a
benchmark record.

## Unchecked constructors

The `unsafe` unchecked constructor lane is a supported opt-in for callers that
independently establish its safety requirements. Maintained benchmarks use it
to match the unchecked reference's constructor work. Application code should
use `try_*` constructors for untrusted input; changing the constructor failure
contract does not eliminate dynamic-tail validation.

## Reproducing the sbe-tool comparators

The benchmarks measure against `sbe/benchmarks/src/sbe_tool_car_patched.rs`
and `sbe_tool_ob_patched.rs`, regenerated from the pinned
`simple-binary-encoding` submodule:

```sh
./scripts/regenerate-sbe-benchmark-reference.sh          # rewrite
./scripts/regenerate-sbe-benchmark-reference.sh --check  # verify, no tracked writes
```

The script verifies the pinned upstream commit and tool version, applies the
documented deterministic patches, and records `sbe-tool-comparator-provenance.json`.
`scripts/regenerate-sbe-tool-reference.sh` regenerates the *wire-parity test*
crates; it is not comparator evidence.
