//! The work accounting beside the percentiles: scheduled, issued, completed,
//! late, rejected and censored, recorded together so a sample that is short of
//! the run's work is never read as a complete one.

use io_bench::matrix;
use io_bench::record::{Record, write_records};
use io_bench::stalls::{StallSummary, format_stalls};
use io_bench::stats::{Work, work_row};

type R = Result<(), Box<dyn std::error::Error>>;

const SEC: i64 = 1_000_000_000;
const T0: i64 = 1_792_000_800 * SEC;

#[test]
fn issued_is_completed_plus_censored_and_scheduled_is_never_below_issued() {
    let w = Work::paced(T0 + 5 * SEC, T0, SEC / 10, 8, 6, 2);
    w.check().expect("the identity holds");
    assert_eq!(
        (
            w.scheduled,
            w.issued,
            w.completed,
            w.late,
            w.rejected,
            w.censored
        ),
        (Some(51), Some(8), Some(6), Some(2), Some(0), Some(2)),
        "censored is the issued work with no sample: 8 issued, 6 recorded"
    );
    assert_eq!(Work::paced(T0, T0, 0, 8, 6, 0).check(), Ok(()));
    // A run whose counts do not add up is named, not silently reported.
    let bad = Work::round_trip(10, 8, 6, 0, 5);
    assert!(bad.check().is_err(), "{bad:?}");
    let behind = Work::round_trip(3, 8, 8, 0, 0);
    assert!(behind.check().is_err(), "{behind:?}");
}

#[test]
fn a_paced_run_counts_every_slot_the_schedule_owed() {
    // 100/s for five seconds is 501 slots from the first due time; the client
    // issued 8 and two of those went out after their slot.
    let w = Work::paced(T0 + 5 * SEC, T0, SEC / 100, 8, 8, 2);
    assert_eq!(w.scheduled, Some(501));
    assert_eq!(
        w.scheduled.zip(w.issued).map(|(s, i)| s - i),
        Some(493),
        "the slots never filled are unsent work"
    );
    // A stall that never caught up: the schedule never slips, so every slot
    // after the stall is a send still owed.
    let stalled = Work::paced(T0 + SEC, T0, SEC / 1000, 10, 10, 10);
    assert_eq!(stalled.scheduled, Some(1001));
    assert_eq!(stalled.late, Some(10));
    assert!(stalled.check().is_ok());
}

#[test]
fn an_unpaced_run_has_no_schedule_beyond_its_replies() {
    let w = Work::paced(T0 + 5 * SEC, 0, 0, 8, 6, 3);
    assert_eq!(w.scheduled, Some(8), "scheduled is the sends made");
    assert_eq!(
        w.late,
        Some(3),
        "kept as recorded, though no slot defines it"
    );
    w.check().expect("the identity holds");
}

#[test]
fn a_receive_only_run_reports_the_send_side_as_not_yet_measured() {
    let w = Work::received(6, 2);
    assert_eq!((w.completed, w.censored), (Some(6), Some(2)));
    // Never a zero: the sender's schedule is not visible from here.
    assert_eq!(
        (w.scheduled, w.issued, w.late, w.rejected),
        (None, None, None, None)
    );
    let cells = w.cells();
    assert_eq!(cells.matches("not yet measured").count(), 4, "{cells}");
    assert!(!cells.contains("| 0 |"), "{cells}");
    w.check().expect("nothing to contradict");
}

#[test]
fn meta_round_trips_the_six_counts_and_a_missing_one_stays_missing() {
    let w = Work::round_trip(10, 8, 6, 2, 2);
    let mut m = String::new();
    w.write_meta(&mut m);
    assert_eq!(
        m,
        "work.scheduled=10\nwork.issued=8\nwork.completed=6\nwork.late=2\nwork.rejected=0\nwork.censored=2\n"
    );
    let map: std::collections::HashMap<String, String> = m
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    assert_eq!(Work::from_meta(&map), Some(w));

    // A partial write is a partial count: the rest is "not yet measured".
    let mut partial = String::new();
    Work::received(6, 2).write_meta(&mut partial);
    let map: std::collections::HashMap<String, String> = partial
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let got = Work::from_meta(&map).expect("two counts are present");
    assert_eq!((got.completed, got.censored), (Some(6), Some(2)));
    assert_eq!(got.issued, None);

    // A run recorded before the counts were has no row, and is not reported as
    // six zeros.
    let empty = std::collections::HashMap::new();
    assert_eq!(Work::from_meta(&empty), None);
}

#[test]
fn the_work_row_puts_the_counts_next_to_the_percentiles() {
    let w = Work::round_trip(10, 8, 6, 2, 2);
    assert_eq!(
        work_row("r1-base-0", &w),
        "| r1-base-0 | 10 | 8 | 6 | 2 | 0 | 2 |"
    );
}

/// A run: `mode`, and records sent at t, decoded `user_us` after sending. The
/// first record sets the run's start; the rest fall after the one-second
/// round-trip warm-up, so the tables report `n` = 6 of the 7 recorded.
fn run(dir: &std::path::Path, mode: &str, user_us: i64, work: &str) -> R {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join("meta.txt"),
        format!("mode={mode}\nduration_s=60\nrate=10000\n{work}"),
    )?;
    let records: Vec<Record> = (0..7)
        .map(|i| {
            let t = if i == 0 {
                T0
            } else {
                T0 + SEC + 100_000 + i * 100_000
            };
            Record {
                id: i + 1,
                src_ns: t,
                src2_ns: if mode == "pingpong" { t - 1_000 } else { 0 },
                user_ns: t + user_us * 1_000,
                ..Record::default()
            }
        })
        .collect();
    write_records(&dir.join("records.bin"), &records)?;
    let sum = StallSummary {
        cpu: 2,
        ..StallSummary::default()
    };
    std::fs::write(dir.join("stalls-0.txt"), format_stalls(&sum, &[]))?;
    Ok(())
}

#[test]
fn a_sample_short_of_the_runs_work_reports_what_it_leaves_out() -> R {
    let dir = std::env::temp_dir().join(format!("io-bench-work-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let run_dir = dir.join("r1-base-0").join("pp-recv-busy-10000");
    // 8 sends out of 10 scheduled slots, 7 of them recorded: 1 censored, 2
    // late. The percentiles then drop the warm-up, so their `n` is 6 — one
    // less than `completed` — and a reader needs both to see it.
    run(
        &run_dir,
        "pingpong",
        200,
        "work.scheduled=10\nwork.issued=8\nwork.completed=7\nwork.late=2\nwork.rejected=0\nwork.censored=1\n",
    )?;
    let (tags, _) = matrix::load(&dir, 10)?;
    assert_eq!(tags.len(), 1);
    let got = tags[0]
        .runs
        .get("pp-recv-busy-10000")
        .expect("the run loads")
        .clone();
    assert_eq!(got.latency.as_ref().map(|s| s.n), Some(6));
    assert_eq!(
        got.work,
        Some(Work::round_trip(10, 8, 7, 2, 1)),
        "the counts reach the report from meta.txt"
    );
    got.work.expect("the counts").check().expect("the identity");

    let report = matrix::report(&tags, &[]);
    assert!(
        report.contains("| r1-base-0 | 10 | 8 | 7 | 2 | 0 | 1 |"),
        "the work counts sit beside the percentiles:\n{report}"
    );
    assert!(
        report.contains("### Work behind the percentiles"),
        "{report}"
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn a_run_without_the_counts_is_not_reported_as_six_zeros() -> R {
    let dir = std::env::temp_dir().join(format!("io-bench-nowork-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let run_dir = dir.join("r1-base-0").join("pp-recv-busy-10000");
    run(&run_dir, "pingpong", 200, "")?;
    let (tags, _) = matrix::load(&dir, 10)?;
    let got = tags[0]
        .runs
        .get("pp-recv-busy-10000")
        .expect("the run loads");
    assert_eq!(got.work, None, "a run recorded before the counts were");
    let report = matrix::report(&tags, &[]);
    assert!(
        !report.contains("### Work behind the percentiles"),
        "no invented counts:\n{report}"
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
