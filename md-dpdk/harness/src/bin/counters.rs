//! Prints the media driver's counters (`AERON_DIR`) whose label contains the
//! filter (default `rusteron-dpdk`: the DPDK transport's TX/RX, ARP and reject
//! counters), one `<value> <type> <label>` line each. A diagnostic for the
//! benchmark lab: where a DPDK frame went missing.
//!
//! counters [filter]
//!
//! `counters loss` prints instead one `key=value;...` line of every counter
//! that records loss or its repair: Aeron's NAKs, retransmits, short sends,
//! loss gap fills, invalid packets and errors, and the DPDK transport's drop
//! counters summed over its ports. lab.sh takes it before and after each run.

// Only `aeron_dir` of the shared helpers is used here.
#[allow(dead_code)]
#[path = "../lab.rs"]
mod lab;

use lab::aeron_dir;
use rusteron_client::*;

/// Aeron's system counters that count loss or its repair, by label prefix.
const AERON_LOSS: [(&str, &str); 8] = [
    ("NAKs sent", "naks-sent"),
    ("NAKs received", "naks-received"),
    ("Retransmits sent", "retransmits-sent"),
    ("Short sends", "short-sends"),
    ("Loss gap fills", "loss-gap-fills"),
    ("Invalid packets", "invalid-packets"),
    ("Errors", "errors"),
    ("Failed offers to ReceiverProxy", "failed-receiver-offers"),
];
/// The DPDK transport's drop counters, by the name ending their label
/// (`missed-packets` mirrors the same NIC miss as `nic-rx-missed`, so only
/// the latter), and `rx-burst-full` and `tx-paced`, saturation signals rather
/// than drops, reported under a `sat-` key.
const DPDK_LOSS: [&str; 11] = [
    "nobufs",
    "tx-eagain",
    "queue-drop",
    "checksum-failure",
    "multi-segment",
    "nic-rx-errors",
    "nic-rx-missed",
    "nic-rx-nombuf",
    "peer-mtu-too-large",
    "rx-burst-full",
    "tx-paced",
];

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_else(|| "rusteron-dpdk".into());
    if filter == "loss" {
        loss();
        return;
    }
    let context = AeronContext::new().unwrap();
    context.set_dir(&aeron_dir()).unwrap();
    let aeron = Aeron::new(&context).unwrap();
    aeron.start().unwrap();
    aeron
        .counters_reader()
        .foreach_counter_fn(|value, _id, type_id, _key, label| {
            if label.contains(filter.as_str()) {
                println!("{value:>12} {type_id:>4} {label}");
            }
        });
}

fn loss() {
    let context = AeronContext::new().unwrap();
    context.set_dir(&aeron_dir()).unwrap();
    let aeron = Aeron::new(&context).unwrap();
    aeron.start().unwrap();
    let mut totals: Vec<(String, i64)> = AERON_LOSS
        .iter()
        .map(|(_, k)| ((*k).to_string(), 0))
        .chain(DPDK_LOSS.iter().map(|k| (dpdk_key(k), 0)))
        .collect();
    aeron
        .counters_reader()
        .foreach_counter_fn(|value, _id, _type_id, _key, label| {
            let key = if label.starts_with("rusteron-dpdk") {
                label
                    .split_whitespace()
                    .find(|word| DPDK_LOSS.contains(word))
                    .map(dpdk_key)
            } else {
                AERON_LOSS
                    .iter()
                    .find(|(prefix, _)| label.starts_with(prefix))
                    .map(|(_, k)| (*k).to_string())
            };
            if let Some(key) = key
                && let Some(total) = totals.iter_mut().find(|(k, _)| *k == key)
            {
                total.1 += value;
            }
        });
    let line: Vec<String> = totals.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("{}", line.join(";"));
}

/// `dpdk-<name>` for a drop, `sat-dpdk-<name>` for the saturation signal.
fn dpdk_key(name: &str) -> String {
    if name == "rx-burst-full" || name == "tx-paced" {
        format!("sat-dpdk-{name}")
    } else {
        format!("dpdk-{name}")
    }
}
