//! Turns result directories into markdown tables.
//!
//! ```text
//! analyse [--warmup-s 10] results/<stamp>/res/lab-... results/<stamp>/res/live-... > tables.md
//! ```
//!
//! Headline metric: the paired `Δuser` (same message, two loops, one clock). In
//! lab runs it is corrected for the replay server's alternating write order.
//! `user − rx_sw` is reported only for reads that delivered exactly one message,
//! because the kernel stamps a read with its newest segment's arrival time.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use io_bench::record::{Record, read_records};
use io_bench::stats::{HEADER, Summary, row, summarize, us};

type BoxResult<T> = Result<T, Box<dyn Error>>;
type Meta = HashMap<String, String>;

fn main() -> ExitCode {
    let mut warmup_s = 10i64;
    let mut dirs: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--warmup-s" {
            warmup_s = it.next().and_then(|v| v.parse().ok()).unwrap_or(10);
        } else {
            dirs.push(a);
        }
    }
    if dirs.is_empty() {
        eprintln!("usage: analyse [--warmup-s N] DIR...");
        return ExitCode::FAILURE;
    }
    let mut out = String::new();
    let mut pingpong_rows = Vec::new();
    let mut summary_rows = Vec::new();
    for d in &dirs {
        if let Err(e) = analyse(
            Path::new(d),
            warmup_s,
            &mut out,
            &mut pingpong_rows,
            &mut summary_rows,
        ) {
            eprintln!("{d}: {e}");
            return ExitCode::FAILURE;
        }
    }
    if !pingpong_rows.is_empty() {
        let _ = writeln!(
            out,
            "## Ping-pong round trip (client send → echo decoded)\n"
        );
        let _ = writeln!(out, "{}", HEADER.replacen("| |", "| variant @ rate |", 1));
        for r in &pingpong_rows {
            let _ = writeln!(out, "{r}");
        }
        let _ = writeln!(out);
    }
    if !summary_rows.is_empty() {
        let mut head = String::from(
            "## Summary: paired runs (µs; Δ = B − A, negative means B decoded first)\n\n\
             | run | A | B | paired msgs | Δuser p50 | Δuser p90 | Δuser p99 | Δrx p50 | Δstack p50 | Δstack p99 | A syscalls/s | B syscalls/s | A kernel CPU % | B kernel CPU % | shared RX queue |\n\
             |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
        );
        for r in &summary_rows {
            head.push_str(r);
            head.push('\n');
        }
        head.push('\n');
        out.insert_str(0, &head);
    }
    print!("{out}");
    ExitCode::SUCCESS
}

fn load_meta(dir: &Path) -> BoxResult<Meta> {
    let text = std::fs::read_to_string(dir.join("meta.txt"))?;
    Ok(text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect())
}

fn get<'a>(m: &'a Meta, k: &str) -> &'a str {
    m.get(k).map_or("", String::as_str)
}

fn num(m: &Meta, k: &str) -> f64 {
    get(m, k).parse().unwrap_or(0.0)
}

fn analyse(
    dir: &Path,
    warmup_s: i64,
    out: &mut String,
    pingpong: &mut Vec<String>,
    summary: &mut Vec<String>,
) -> BoxResult<()> {
    let meta = load_meta(dir)?;
    let all = read_records(&dir.join("records.bin"))?;
    let mode = get(&meta, "mode").to_owned();
    let variants: Vec<String> = get(&meta, "variants")
        .split(',')
        .map(str::to_owned)
        .collect();
    let name = dir.file_name().map_or_else(
        || dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );

    if mode == "pingpong" {
        // Ping-pong runs are short; keep every round trip but the first second.
        let t0 = all.iter().map(|r| r.user_ns).min().unwrap_or(0);
        let mut rtt: Vec<i64> = all
            .iter()
            .filter(|r| r.user_ns >= t0 + 1_000_000_000)
            .map(|r| r.user_ns - r.src_ns)
            .collect();
        if let Some(s) = summarize(&mut rtt) {
            pingpong.push(row(
                &format!("{} @ {}/s", variants[0], get(&meta, "rate")),
                &s,
            ));
        }
        // HTTP client runs (http_bench) carry no loop counters.
        if meta.contains_key("loop.0.spins") {
            loop_table(&meta, &variants, out, &name);
        }
        return Ok(());
    }

    // Drop the warm-up: the backlog delivered as the loops start, first-touch
    // effects.
    let t0 = all.iter().map(|r| r.user_ns).min().unwrap_or(0);
    let recs: Vec<Record> = all
        .into_iter()
        .filter(|r| r.user_ns >= t0 + warmup_s * 1_000_000_000)
        .collect();
    let streams: Vec<String> = get(&meta, "streams")
        .split(',')
        .map(str::to_owned)
        .collect();
    let aa = variants.len() == 2 && variants[0] == variants[1];
    let _ = writeln!(
        out,
        "## {name}: {mode}{}, {} s (first {warmup_s} s dropped), {} connection(s) per variant, kernel {}\n",
        if aa { " (A/A control)" } else { "" },
        get(&meta, "duration_s"),
        get(&meta, "conns_per_variant"),
        get(&meta, "kernel"),
    );
    let _ = writeln!(
        out,
        "busy_poll_us={} wait_us={} mlockall={} BusyPollRxPackets during run={} clocksource={}\n",
        get(&meta, "busy_poll_us"),
        get(&meta, "wait_us"),
        get(&meta, "mlockall"),
        get(&meta, "busy_poll_rx_packets"),
        get(&meta, "clocksource")
    );
    loop_table(&meta, &variants, out, &name);
    queue_table(&meta, &variants, out);
    copy_table(&meta, &variants, out);

    let label = |v: usize| -> String {
        if aa {
            format!("{} #{v}", variants[v])
        } else {
            variants[v].clone()
        }
    };
    let by_variant = |v: usize| {
        recs.iter()
            .filter(move |r| usize::from(r.variant) == v && r.rx_sw_ns > 0)
    };
    let mut per_read: HashMap<(u8, u64), u32> = HashMap::new();
    for r in &recs {
        *per_read.entry((r.variant, r.read_seq)).or_insert(0) += 1;
    }
    let single = |r: &Record| per_read.get(&(r.variant, r.read_seq)) == Some(&1);

    let _ = writeln!(
        out,
        "### Receive stack: kernel rx timestamp → message decoded (`user − rx_sw`), single-message reads only\n\n\
         The kernel stamps a read with its newest segment, so multi-message reads are excluded. Interrupt-driven \
         and polled variants take the rx timestamp on different CPUs, so compare this only within a regime.\n"
    );
    let _ = writeln!(out, "{HEADER}");
    for v in 0..variants.len() {
        let n_all = by_variant(v).count();
        let mut one: Vec<i64> = by_variant(v)
            .filter(|r| single(r))
            .map(|r| r.user_ns - r.rx_sw_ns)
            .collect();
        let n_one = one.len();
        if let Some(s) = summarize(&mut one) {
            let _ = writeln!(
                out,
                "{}",
                row(
                    &format!("{} ({n_one} of {n_all} in 1-msg reads)", label(v)),
                    &s
                )
            );
        }
    }
    let _ = writeln!(out);

    if mode == "binance" {
        let _ = writeln!(
            out,
            "### Binance event time → kernel rx (`rx_sw − E`)\n\nIncludes Binance's publish delay, the Azure→AWS path and \
             the clock offset; context only, not a loop comparison.\n"
        );
        let _ = writeln!(out, "{HEADER}");
        for (si, sname) in streams.iter().enumerate() {
            for v in 0..variants.len() {
                let mut wire: Vec<i64> = by_variant(v)
                    .filter(|r| usize::from(r.stream) == si && r.src_ns > 0)
                    .map(|r| r.rx_sw_ns - r.src_ns)
                    .collect();
                if let Some(s) = summarize(&mut wire) {
                    let _ = writeln!(out, "{}", row(&format!("{sname} {}", label(v)), &s));
                }
            }
        }
        let _ = writeln!(out);
    }

    if let Some(p) = paired(&recs, &variants, aa, mode == "lab", &single, out) {
        let k = |v: usize, f: &str| num(&meta, &format!("loop.{v}.{f}"));
        let dur = num(&meta, "duration_s").max(1.0);
        let kern = |v: usize| {
            let c = k(v, "utime_ticks") + k(v, "stime_ticks");
            if c > 0.0 {
                100.0 * k(v, "stime_ticks") / c
            } else {
                0.0
            }
        };
        let shared = shares_queue(&meta, &variants);
        summary.push(format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.0} | {:.0} | {:.1} | {:.1} | {} |",
            variants[0],
            variants[1],
            p.n,
            us(p.user.p50),
            us(p.user.p90),
            us(p.user.p99),
            p.rx.map_or_else(String::new, |s| us(s.p50)),
            p.stack.map_or_else(String::new, |s| us(s.p50)),
            p.stack.map_or_else(String::new, |s| us(s.p99)),
            k(0, "syscalls") / dur,
            k(1, "syscalls") / dur,
            kern(0),
            kern(1),
            if shared { "yes" } else { "no" }
        ));
    }
    Ok(())
}

fn shares_queue(meta: &Meta, variants: &[String]) -> bool {
    let ids = |v: usize| -> Vec<String> {
        (0..)
            .map_while(|c| meta.get(&format!("conn.{v}.{c}")))
            .map(|val| conn_field(val, "napi_id"))
            .filter(|i| i != "0" && !i.is_empty())
            .collect()
    };
    variants.len() == 2 && ids(0).iter().any(|i| ids(1).contains(i))
}

/// Headline numbers of the first paired comparison.
struct PairedHeadline {
    n: usize,
    user: Summary,
    rx: Option<Summary>,
    stack: Option<Summary>,
}

fn loop_table(meta: &Meta, variants: &[String], out: &mut String, name: &str) {
    let dur = num(meta, "duration_s").max(1.0);
    let _ = writeln!(out, "### Loop counters ({name})\n");
    let _ = writeln!(
        out,
        "| variant | messages | reads | msgs/read | syscalls | syscalls/msg | syscalls/s | spins/s | recv EAGAIN % | \
         kernel CPU % | NIC polling set up | sleeps (vol. ctx sw) | preempted | longest iteration µs | iterations >5 / >20 / >100 µs | dropped | unparsed |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for (v, vname) in variants.iter().enumerate() {
        let k = |f: &str| num(meta, &format!("loop.{v}.{f}"));
        let msgs = k("messages");
        let reads = k("completions");
        let sys = k("syscalls");
        let recv = k("recvmsg_calls");
        let eagain = if recv > 0.0 {
            100.0 * k("recvmsg_eagain") / recv
        } else {
            0.0
        };
        let cpu = k("utime_ticks") + k("stime_ticks");
        let kern = if cpu > 0.0 {
            100.0 * k("stime_ticks") / cpu
        } else {
            0.0
        };
        let _ = writeln!(
            out,
            "| {vname} | {msgs:.0} | {reads:.0} | {:.2} | {sys:.0} | {:.2} | {:.0} | {:.0} | {eagain:.1} | {kern:.1} | {} | {} | {} | {} | {} / {} / {} | {:.0} | {:.0} |",
            if reads > 0.0 { msgs / reads } else { 0.0 },
            if msgs > 0.0 { sys / msgs } else { 0.0 },
            sys / dur,
            k("spins") / dur,
            get(meta, &format!("loop.{v}.napi")),
            get(meta, &format!("loop.{v}.vol_ctx_switches")),
            get(meta, &format!("loop.{v}.invol_ctx_switches")),
            get(meta, &format!("loop.{v}.gap_max_us")),
            get(meta, &format!("loop.{v}.gaps_over_5us")),
            get(meta, &format!("loop.{v}.gaps_over_20us")),
            get(meta, &format!("loop.{v}.gaps_over_100us")),
            k("dropped"),
            k("unparsed")
        );
    }
    let _ = writeln!(out);
}

fn conn_field(val: &str, f: &str) -> String {
    val.split(' ')
        .find_map(|p| p.strip_prefix(&format!("{f}:")).map(str::to_owned))
        .unwrap_or_default()
}

/// Per-socket NIC queue (NAPI id) and the CPU that handled its last packet, with a
/// flag when the two variants of a pair share a queue (then one side's polling
/// also delivers the other side's packets).
fn queue_table(meta: &Meta, variants: &[String], out: &mut String) {
    let mut ids: Vec<Vec<String>> = vec![Vec::new(); variants.len()];
    let mut rows = Vec::new();
    for (v, vname) in variants.iter().enumerate() {
        for c in 0.. {
            let Some(val) = meta.get(&format!("conn.{v}.{c}")) else {
                break;
            };
            let id = conn_field(val, "napi_id");
            rows.push(format!(
                "| {vname} | {c} | {id} | {} |",
                conn_field(val, "rx_cpu")
            ));
            ids[v].push(id);
        }
    }
    let shared = variants.len() == 2 && ids[0].iter().any(|i| i != "0" && ids[1].contains(i));
    let _ = writeln!(
        out,
        "### NIC queues\n\nThe pair {} an RX queue.\n\n| variant | conn | NAPI id | last rx CPU |\n|---|---|---|---|",
        if shared {
            "**shares**"
        } else {
            "does not share"
        }
    );
    for r in rows {
        let _ = writeln!(out, "{r}");
    }
    let _ = writeln!(out);
}

fn copy_table(meta: &Meta, variants: &[String], out: &mut String) {
    let _ = writeln!(
        out,
        "### Copies beyond the single kernel→buffer copy\n\n| variant | bytes received | span-copy bytes | compaction bytes | \
         fragment bytes | extra copied % |\n|---|---|---|---|---|---|"
    );
    for (v, vname) in variants.iter().enumerate() {
        let mut tot = [0f64; 4];
        for (k, val) in meta {
            if !k.starts_with(&format!("conn.{v}.")) {
                continue;
            }
            for (i, f) in [
                "bytes_in",
                "span_copy_bytes",
                "compact_bytes",
                "frag_copy_bytes",
            ]
            .iter()
            .enumerate()
            {
                tot[i] += conn_field(val, f).parse::<f64>().unwrap_or(0.0);
            }
        }
        let extra = if tot[0] > 0.0 {
            100.0 * (tot[1] + tot[2] + tot[3]) / tot[0]
        } else {
            0.0
        };
        let _ = writeln!(
            out,
            "| {vname} | {:.0} | {:.0} | {:.0} | {:.0} | {extra:.3} |",
            tot[0], tot[1], tot[2], tot[3]
        );
    }
    let _ = writeln!(out);
}

fn paired(
    recs: &[Record],
    variants: &[String],
    aa: bool,
    lab: bool,
    single: &dyn Fn(&Record) -> bool,
    out: &mut String,
) -> Option<PairedHeadline> {
    if variants.len() < 2 {
        return None;
    }
    let mut headline = None;
    let mut base: HashMap<(u16, i64), &Record> = HashMap::new();
    for r in recs.iter().filter(|r| r.variant == 0 && r.rx_sw_ns > 0) {
        base.entry((r.stream, r.id)).or_insert(r);
    }
    let _ = writeln!(
        out,
        "### Paired by message, relative to {}{}\n\n**Δuser** = decoded-time difference for the same message \
         (negative: this variant decoded first). {}\nDiagnostics: Δrx = kernel-arrival difference; Δstack = Δuser − Δrx \
         over messages that were alone in their read on both sides.\n",
        variants[0],
        if aa {
            " (A/A: any difference is noise, core or connection bias)"
        } else {
            ""
        },
        if lab {
            "The replay server writes each message to both connections back to back, alternating which goes first; \
             the corrected row removes that ±offset (half the gap between the even- and odd-sequence medians)."
        } else {
            "Live: each variant has its own Binance connection, so Δuser also contains Binance's per-connection send \
             skew; use it as confirmation of the lab result, not as a verdict."
        }
    );
    let _ = writeln!(out, "{HEADER}");
    for v in 1..variants.len() {
        let mut du = Vec::new();
        let mut dr = Vec::new();
        let mut ds = Vec::new();
        let mut parity: [Vec<i64>; 2] = [Vec::new(), Vec::new()];
        let mut first = 0usize;
        let mut seen = HashMap::new();
        for r in recs
            .iter()
            .filter(|r| usize::from(r.variant) == v && r.rx_sw_ns > 0)
        {
            if seen.insert((r.stream, r.id), ()).is_some() {
                continue;
            }
            let Some(b) = base.get(&(r.stream, r.id)) else {
                continue;
            };
            let d_user = r.user_ns - b.user_ns;
            let d_rx = r.rx_sw_ns - b.rx_sw_ns;
            du.push(d_user);
            dr.push(d_rx);
            if single(r) && single(b) {
                ds.push(d_user - d_rx);
            }
            if d_user < 0 {
                first += 1;
            }
            if lab {
                parity[(r.id & 1) as usize].push(d_user);
            }
        }
        let n = du.len();
        let vl = if aa {
            format!("{} #{v}", variants[v])
        } else {
            variants[v].clone()
        };
        let emit = |out: &mut String, l: &str, s: Option<Summary>| {
            if let Some(s) = s {
                let _ = writeln!(out, "{}", row(l, &s));
            }
        };
        let stack_summary = summarize(&mut ds.clone());
        let rx_summary = summarize(&mut dr.clone());
        if lab && !parity[0].is_empty() && !parity[1].is_empty() {
            let me = summarize(&mut parity[0].clone()).map_or(0, |s| s.p50);
            let mo = summarize(&mut parity[1].clone()).map_or(0, |s| s.p50);
            let off = (me - mo) / 2;
            let mut corrected: Vec<i64> = parity[0]
                .iter()
                .map(|d| d - off)
                .chain(parity[1].iter().map(|d| d + off))
                .collect();
            let cs = summarize(&mut corrected);
            if headline.is_none() {
                headline = cs.map(|user| PairedHeadline {
                    n,
                    user,
                    rx: rx_summary,
                    stack: stack_summary,
                });
            }
            emit(out, &format!("**{vl} Δuser (write-order corrected)**"), cs);
            emit(
                out,
                &format!("{vl} Δuser, even seq"),
                summarize(&mut parity[0]),
            );
            emit(
                out,
                &format!("{vl} Δuser, odd seq"),
                summarize(&mut parity[1]),
            );
        } else {
            let us_ = summarize(&mut du);
            if headline.is_none() {
                headline = us_.map(|user| PairedHeadline {
                    n,
                    user,
                    rx: rx_summary,
                    stack: stack_summary,
                });
            }
            emit(out, &format!("**{vl} Δuser**"), us_);
        }
        emit(out, &format!("{vl} Δrx"), summarize(&mut dr));
        emit(
            out,
            &format!("{vl} Δstack (1-msg reads)"),
            summarize(&mut ds),
        );
        let _ = writeln!(
            out,
            "\n{vl}: {n} paired messages; decoded before {} on {:.1} % (uncorrected).\n",
            variants[0],
            if n > 0 {
                100.0 * first as f64 / n as f64
            } else {
                0.0
            }
        );
        if v + 1 < variants.len() {
            let _ = writeln!(out, "{HEADER}");
        }
    }
    headline
}
