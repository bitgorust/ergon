//! The driver with every backend over loopback: delivery, sending, peer close,
//! per-connection protocol errors, stale ids and drop with I/O in flight.
#![cfg(target_os = "linux")]
#![allow(missing_docs)]

mod support;

use std::time::{Duration, Instant};

use ergon_io::{
    Backend, Config, ConnId, Driver, Error, Handler, RxMeta, WsConn, WsDial, WsHandler, ws,
};
use support::{Lab, R};

#[derive(Default)]
struct Collect {
    msgs: Vec<(ConnId, u8, Vec<u8>)>,
    closed: Vec<(ConnId, String)>,
    opened: Vec<ConnId>,
}

impl Handler for Collect {
    fn on_open(&mut self, id: ConnId) {
        self.opened.push(id);
    }

    fn on_closed(&mut self, id: ConnId, error: Error) {
        self.closed.push((id, error.to_string()));
    }
}

impl WsHandler for Collect {
    fn on_message(&mut self, id: ConnId, opcode: u8, payload: &[u8], _: &RxMeta, _: u16) {
        self.msgs.push((id, opcode, payload.to_vec()));
    }
}

fn driver(b: Backend) -> R<Driver<WsConn>> {
    // Short waits keep the sleeping backends quick in tests.
    let mut cfg = Config::new(b);
    cfg.wait_us = cfg.wait_us.min(100);
    Ok(Driver::new(cfg)?)
}

fn poll_until(d: &mut Driver<WsConn>, h: &mut Collect, done: impl Fn(&Collect) -> bool) -> R {
    let end = Instant::now() + Duration::from_secs(10);
    while !done(h) {
        d.poll(h)?;
        if Instant::now() > end {
            return Err(format!(
                "timed out: {} msgs, closed {:?}, stats {:?}",
                h.msgs.len(),
                h.closed,
                d.stats()
            )
            .into());
        }
    }
    Ok(())
}

fn each_backend(test: impl Fn(Backend) -> R) -> R {
    for b in Backend::ALL {
        eprintln!("backend {b}");
        test(b).map_err(|e| format!("{b}: {e}"))?;
    }
    Ok(())
}

#[test]
fn delivers_in_order_then_reports_peer_close() -> R {
    each_backend(|b| {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                let mut p = lab.accept()?;
                for i in 0..500u32 {
                    p.send(ws::OP_TEXT, true, format!("m{i}").as_bytes())?;
                }
                p.send(ws::OP_CLOSE, true, &[])?;
                Ok(())
            });
            let id = d.add(WsConn::connect(
                &lab.target("/")?,
                lab.client_cfg.clone(),
                1 << 20,
            )?)?;
            let mut h = Collect::default();
            poll_until(&mut d, &mut h, |h| !h.closed.is_empty())?;
            srv.join().map_err(|_| "server panicked")??;
            assert_eq!(
                h.msgs.len(),
                500,
                "closed {:?} stats {:?}",
                h.closed,
                d.stats()
            );
            for (i, (cid, op, body)) in h.msgs.iter().enumerate() {
                assert_eq!((*cid, *op), (id, ws::OP_TEXT));
                assert_eq!(body, format!("m{i}").as_bytes());
            }
            assert_eq!(h.closed.len(), 1);
            assert_eq!(h.closed[0].0, id);
            assert!(d.get(id).is_none() && d.is_empty());
            assert_eq!(d.stats().closed, 1);
            Ok(())
        })
    })
}

#[test]
fn sends_and_receives_echo() -> R {
    each_backend(|b| {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                let mut p = lab.accept()?;
                for _ in 0..50 {
                    let (op, body) = p.recv()?;
                    p.send(op, true, &body)?;
                }
                Ok(())
            });
            let id = d.add(WsConn::connect(
                &lab.target("/")?,
                lab.client_cfg.clone(),
                1 << 20,
            )?)?;
            let mut h = Collect::default();
            for i in 0..50usize {
                let body = vec![u8::try_from(i)?; 1 + i * 100];
                d.get_mut(id)
                    .ok_or("conn gone")?
                    .send_frame(ws::OP_BINARY, &body)?;
                d.flush()?;
                poll_until(&mut d, &mut h, |h| h.msgs.len() > i)?;
                assert_eq!(h.msgs[i].2, body);
            }
            srv.join().map_err(|_| "server panicked")??;
            Ok(())
        })
    })
}

#[test]
fn protocol_error_closes_only_that_connection() -> R {
    each_backend(|b| {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                let mut bad = lab.accept()?;
                let mut good = lab.accept()?;
                // Servers must not mask: the client treats it as a protocol error.
                let mut masked = vec![0u8; ws::header_len(3, true) + 3];
                ws::encode_frame(ws::OP_TEXT, b"bad", Some([1, 2, 3, 4]), &mut masked)?;
                bad.write(&masked)?;
                for _ in 0..20 {
                    good.send(ws::OP_TEXT, true, b"ok")?;
                }
                assert!(
                    bad.client_closed()?,
                    "the failed connection's socket is closed"
                );
                Ok(())
            });
            let t = lab.target("/")?;
            let bad = d.add(WsConn::connect(&t, lab.client_cfg.clone(), 1 << 20)?)?;
            let good = d.add(WsConn::connect(&t, lab.client_cfg.clone(), 1 << 20)?)?;
            let mut h = Collect::default();
            poll_until(&mut d, &mut h, |h| {
                h.msgs.len() == 20 && h.closed.len() == 1
            })?;
            assert_eq!(h.closed[0].0, bad);
            assert!(h.closed[0].1.contains("masked"), "{}", h.closed[0].1);
            assert!(h.msgs.iter().all(|m| m.0 == good));
            assert!(d.get(bad).is_none() && d.get(good).is_some());
            // The closing connection's buffers are released on a later poll.
            for _ in 0..1000 {
                d.poll(&mut h)?;
            }
            srv.join().map_err(|_| "server panicked")??;
            Ok(())
        })
    })
}

#[test]
fn stale_id_never_reaches_a_reused_slot() -> R {
    each_backend(|b| {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                let mut first = lab.accept()?;
                let mut second = lab.accept()?;
                assert!(first.client_closed()?);
                second.send(ws::OP_TEXT, true, b"second")?;
                assert!(second.client_closed()?);
                Ok(())
            });
            let target = lab.target("/")?;
            let old = d.add(WsConn::connect(&target, lab.client_cfg.clone(), 1 << 20)?)?;
            d.close(old);
            let mut h = Collect::default();
            // Let io_uring release the slot, then reuse it.
            for _ in 0..1000 {
                d.poll(&mut h)?;
            }
            let new = d.add(WsConn::connect(&target, lab.client_cfg.clone(), 1 << 20)?)?;
            assert_eq!(old.index(), new.index(), "the slot is reused");
            assert_ne!(old, new);
            assert!(d.get(old).is_none() && d.get_mut(old).is_none());
            d.close(old); // stale: must not close the new connection
            assert!(d.get(new).is_some());
            poll_until(&mut d, &mut h, |h| h.msgs.len() == 1)?;
            assert_eq!(h.msgs[0].0, new);
            assert!(h.closed.is_empty(), "{:?}", h.closed);
            drop(d);
            srv.join().map_err(|_| "server panicked")??;
            Ok(())
        })
    })
}

#[test]
fn drop_with_io_in_flight_closes_sockets() -> R {
    each_backend(|b| {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                let mut p = lab.accept()?;
                p.send(ws::OP_TEXT, true, b"one")?;
                assert!(p.client_closed()?, "dropping the driver closes the socket");
                Ok(())
            });
            let id = d.add(WsConn::connect(
                &lab.target("/")?,
                lab.client_cfg.clone(),
                1 << 20,
            )?)?;
            let mut h = Collect::default();
            poll_until(&mut d, &mut h, |h| h.msgs.len() == 1)?;
            // A receive is armed (io_uring) and a send is queued and in flight.
            d.get_mut(id)
                .ok_or("conn gone")?
                .send_frame(ws::OP_TEXT, &[7; 12_000])?;
            d.poll(&mut h)?;
            drop(d);
            srv.join().map_err(|_| "server panicked")??;
            Ok(())
        })
    })
}

#[test]
fn spin_backends_never_sleep_when_idle() -> R {
    for b in Backend::ALL.into_iter().filter(|b| !b.may_sleep()) {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                let mut p = lab.accept()?;
                assert!(p.client_closed()?);
                Ok(())
            });
            d.add(WsConn::connect(
                &lab.target("/")?,
                lab.client_cfg.clone(),
                1 << 20,
            )?)?;
            let mut h = Collect::default();
            let (v0, _) = ergon_io::clock::thread_ctx_switches();
            for _ in 0..200_000 {
                d.poll(&mut h)?;
            }
            let (v1, _) = ergon_io::clock::thread_ctx_switches();
            assert_eq!(v1 - v0, 0, "{b} slept while idle");
            drop(d);
            srv.join().map_err(|_| "server panicked")??;
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn connect_never_blocks_the_poll_loop() -> R {
    each_backend(|b| {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                // A slow server: 150 ms before the handshake even starts.
                let (sock, _) = lab.listener.accept()?;
                std::thread::sleep(Duration::from_millis(150));
                let mut p = support::Peer::serve(&lab, sock)?;
                p.send(ws::OP_TEXT, true, b"hi")?;
                assert!(p.client_closed()?);
                Ok(())
            });
            let t0 = Instant::now();
            let id = d.connect(WsDial::new(
                lab.target("/")?,
                lab.client_cfg.clone(),
                1 << 20,
            )?)?;
            assert!(
                t0.elapsed() < Duration::from_millis(5),
                "connect returned late"
            );
            assert!(d.get(id).is_none(), "not open yet");
            let mut h = Collect::default();
            let mut slowest = Duration::ZERO;
            let end = Instant::now() + Duration::from_secs(10);
            while h.msgs.is_empty() {
                let t = Instant::now();
                d.poll(&mut h)?;
                slowest = slowest.max(t.elapsed());
                assert!(Instant::now() < end, "never opened: {:?}", h.closed);
            }
            assert_eq!(h.opened, vec![id]);
            assert_eq!(h.msgs[0].0, id);
            assert!(d.get(id).is_some());
            // The worst poll is a handshake step's CPU, never the network wait.
            assert!(
                slowest < Duration::from_millis(20),
                "a poll took {slowest:?}"
            );
            assert_eq!(d.stats().connects, 1);
            drop(d);
            srv.join().map_err(|_| "server panicked")??;
            Ok(())
        })
    })
}

#[test]
fn connect_times_out_and_cleans_up() -> R {
    let lab = Lab::new()?;
    let mut cfg = Config::new(Backend::EpollSpin);
    cfg.connect_timeout_ms = 200;
    let mut d: Driver<WsConn> = Driver::new(cfg)?;
    std::thread::scope(|s| -> R {
        let srv = s.spawn(|| -> R {
            // Accepts but never speaks TLS.
            let (mut sock, _) = lab.listener.accept()?;
            let mut b = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut sock, &mut b) {
                    // The client closed its socket (a reset if it left data unread).
                    Ok(0) => return Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => return Ok(()),
                    Ok(_) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        });
        let id = d.connect(WsDial::new(
            lab.target("/")?,
            lab.client_cfg.clone(),
            1 << 20,
        )?)?;
        let mut h = Collect::default();
        poll_until(&mut d, &mut h, |h| !h.closed.is_empty())?;
        assert_eq!(h.closed[0].0, id);
        assert!(h.closed[0].1.contains("timed out"), "{}", h.closed[0].1);
        assert!(h.opened.is_empty() && d.connecting() == 0);
        assert_eq!(d.stats().connect_failures, 1);
        srv.join().map_err(|_| "server panicked")??;
        Ok(())
    })
}

#[test]
fn closing_while_connecting_drops_the_socket() -> R {
    let lab = Lab::new()?;
    let mut d = driver(Backend::RecvBusy)?;
    std::thread::scope(|s| -> R {
        let srv = s.spawn(|| -> R {
            let (mut sock, _) = lab.listener.accept()?;
            let mut b = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut sock, &mut b) {
                    Ok(0) => return Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => return Ok(()),
                    Ok(_) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        });
        let id = d.connect(WsDial::new(
            lab.target("/")?,
            lab.client_cfg.clone(),
            1 << 20,
        )?)?;
        let mut h = Collect::default();
        for _ in 0..100 {
            d.poll(&mut h)?;
        }
        d.close(id);
        assert_eq!(d.connecting(), 0);
        srv.join().map_err(|_| "server panicked")??;
        // A new connection does not inherit the closed id.
        assert!(d.get(id).is_none());
        assert!(h.opened.is_empty() && h.closed.is_empty());
        Ok(())
    })
}

#[test]
fn frames_sent_with_the_upgrade_then_more_on_every_backend() -> R {
    each_backend(|b| {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                let (sock, _) = lab.listener.accept()?;
                let mut p = support::Peer::serve_with(&lab, sock, &[b"first", b"second"])?;
                // Later traffic lands where the armed receive points.
                std::thread::sleep(Duration::from_millis(20));
                for i in 0..50u32 {
                    p.send(ws::OP_TEXT, true, format!("m{i}").as_bytes())?;
                }
                assert!(p.client_closed()?);
                Ok(())
            });
            let id = d.connect(WsDial::new(
                lab.target("/")?,
                lab.client_cfg.clone(),
                1 << 20,
            )?)?;
            let mut h = Collect::default();
            poll_until(&mut d, &mut h, |h| h.msgs.len() == 52)?;
            assert_eq!(h.msgs[0].2, b"first");
            assert_eq!(h.msgs[1].2, b"second");
            assert_eq!(h.msgs[51].2, b"m49");
            assert!(h.closed.is_empty(), "{:?}", h.closed);
            assert!(d.get(id).is_some());
            drop(d);
            srv.join().map_err(|_| "server panicked")??;
            Ok(())
        })
    })
}

#[test]
fn background_connect_keeps_handshake_cpu_off_the_loop() -> R {
    for b in [Backend::RecvBusy, Backend::UringSpin] {
        let lab = Lab::new()?;
        let mut d = driver(b)?;
        std::thread::scope(|s| -> R {
            let srv = s.spawn(|| -> R {
                let mut p = lab.accept()?;
                p.send(ws::OP_TEXT, true, b"hi")?;
                assert!(p.client_closed()?);
                Ok(())
            });
            let id = d.connect_background(WsDial::new(
                lab.target("/")?,
                lab.client_cfg.clone(),
                1 << 20,
            )?)?;
            let mut h = Collect::default();
            let mut slowest = Duration::ZERO;
            let end = Instant::now() + Duration::from_secs(10);
            while h.msgs.is_empty() {
                let t = Instant::now();
                d.poll(&mut h)?;
                slowest = slowest.max(t.elapsed());
                assert!(Instant::now() < end, "never opened: {:?}", h.closed);
            }
            assert_eq!(h.opened, vec![id]);
            assert_eq!(h.msgs[0].0, id);
            assert_eq!(
                d.stats().connect_step_max_ns,
                0,
                "no set-up step ran on the loop"
            );
            assert!(
                slowest < Duration::from_millis(5),
                "{b}: a poll took {slowest:?}"
            );
            drop(d);
            srv.join().map_err(|_| "server panicked")??;
            Ok(())
        })
        .map_err(|e| format!("{b}: {e}"))?;
    }
    Ok(())
}

#[test]
fn background_connect_closed_early_is_discarded() -> R {
    let lab = Lab::new()?;
    let mut d = driver(Backend::EpollSpin)?;
    std::thread::scope(|s| -> R {
        let srv = s.spawn(|| -> R {
            let (sock, _) = lab.listener.accept()?;
            std::thread::sleep(Duration::from_millis(100));
            let mut p = support::Peer::serve(&lab, sock)?;
            assert!(p.client_closed()?, "the discarded connection is closed");
            Ok(())
        });
        let id = d.connect_background(WsDial::new(
            lab.target("/")?,
            lab.client_cfg.clone(),
            1 << 20,
        )?)?;
        d.close(id);
        let mut h = Collect::default();
        let end = Instant::now() + Duration::from_millis(500);
        while Instant::now() < end {
            d.poll(&mut h)?;
        }
        assert!(h.opened.is_empty() && h.closed.is_empty());
        assert!(d.get(id).is_none() && d.is_empty());
        srv.join().map_err(|_| "server panicked")??;
        Ok(())
    })
}

#[test]
fn cpu_lists_parse() {
    use ergon_io::sys::parse_cpu_list;
    assert_eq!(parse_cpu_list("0-3,6,8-9\n"), vec![0, 1, 2, 3, 6, 8, 9]);
    assert_eq!(parse_cpu_list(""), Vec::<usize>::new());
    assert_eq!(parse_cpu_list("5"), vec![5]);
}

#[test]
fn background_helper_leaves_the_polling_cpu() -> R {
    let online =
        ergon_io::sys::parse_cpu_list(&std::fs::read_to_string("/sys/devices/system/cpu/online")?);
    assert!(online.len() > 1, "test needs more than one CPU");
    let poller = online[0];
    let lab = Lab::new()?;
    let lab_ref = &lab;
    std::thread::scope(|s| -> R {
        // The poller pins itself, as a busy-spin runtime would: the helper
        // inherits that single-CPU affinity unless it moves.
        let client = s.spawn(move || -> R {
            let lab = lab_ref;
            ergon_io::clock::pin_current_thread(poller)?;
            let mut d = driver(Backend::EpollSpin)?;
            d.connect_background(WsDial::new(
                lab.target("/")?,
                lab.client_cfg.clone(),
                1 << 20,
            )?)?;
            let mut h = Collect::default();
            poll_until(&mut d, &mut h, |h| !h.opened.is_empty())?;
            let helper = std::fs::read_dir("/proc/self/task")?
                .filter_map(std::result::Result::ok)
                .map(|e| e.path())
                .find(|p| {
                    std::fs::read_to_string(p.join("comm"))
                        .is_ok_and(|c| c.trim() == "ergon-io-dial")
                })
                .ok_or("helper thread not found")?;
            let status = std::fs::read_to_string(helper.join("status"))?;
            let allowed = status
                .lines()
                .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
                .map(ergon_io::sys::parse_cpu_list)
                .ok_or("no Cpus_allowed_list")?;
            assert!(
                !allowed.is_empty() && !allowed.contains(&poller),
                "helper may run on the polling CPU: {allowed:?}"
            );
            Ok(())
        });
        let mut p = lab.accept()?;
        p.send(ws::OP_TEXT, true, b"x")?;
        client.join().map_err(|_| "client panicked")??;
        Ok(())
    })
}

#[test]
fn sqpoll_thread_never_shares_the_polling_cpu() -> R {
    let online =
        ergon_io::sys::parse_cpu_list(&std::fs::read_to_string("/sys/devices/system/cpu/online")?);
    assert!(online.len() > 1, "test needs more than one CPU");
    let poller = online[0];
    std::thread::scope(|s| -> R {
        s.spawn(move || -> R {
            ergon_io::clock::pin_current_thread(poller)?;
            // SQPOLL threads of earlier tests may still be exiting: only the
            // one this driver creates counts.
            let sqpoll_threads = || -> Vec<std::path::PathBuf> {
                std::fs::read_dir("/proc/self/task")
                    .into_iter()
                    .flatten()
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        std::fs::read_to_string(p.join("comm"))
                            .is_ok_and(|c| c.starts_with("iou-sqp"))
                    })
                    .collect()
            };
            let before = sqpoll_threads();
            // No sqpoll_cpu configured: the default must avoid the poller's CPU.
            let _d: Driver<WsConn> = Driver::new(Config::new(Backend::UringSqpoll))?;
            // The kernel thread names itself once it runs: look for up to 1 s.
            let deadline = Instant::now() + Duration::from_secs(1);
            let sq = loop {
                if let Some(p) = sqpoll_threads().into_iter().find(|p| !before.contains(p)) {
                    break p;
                }
                if Instant::now() > deadline {
                    return Err("SQPOLL thread not found".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            let status = std::fs::read_to_string(sq.join("status"))?;
            let allowed = status
                .lines()
                .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
                .map(ergon_io::sys::parse_cpu_list)
                .ok_or("no Cpus_allowed_list")?;
            assert!(
                !allowed.is_empty() && !allowed.contains(&poller),
                "SQPOLL thread may run on the polling CPU: {allowed:?}"
            );
            Ok(())
        })
        .join()
        .map_err(|_| "panicked")?
    })
}
