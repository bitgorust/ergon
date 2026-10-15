//! `HttpClient`: the native pool and (feature `curl`) libcurl behind one API.
#![cfg(target_os = "linux")]
#![allow(missing_docs)]

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ergon_io::{ClientHandler, Error, HttpClient, HttpClientConfig, HttpImpl, Response, Signer};
use support::{Lab, R};

#[derive(Default)]
struct Seen {
    ok: Vec<(u64, u16, String)>,
    errors: Vec<(u64, String)>,
}

impl ClientHandler for Seen {
    fn on_response(&mut self, request: u64, r: &Response<'_>) {
        self.ok.push((
            request,
            r.status,
            String::from_utf8_lossy(r.body).into_owned(),
        ));
    }
    fn on_error(&mut self, request: u64, error: Error) {
        self.errors.push((request, error.to_string()));
    }
}

fn poll_until(c: &mut HttpClient, h: &mut Seen, done: impl Fn(&HttpClient, &Seen) -> bool) -> R {
    let end = Instant::now() + Duration::from_secs(10);
    while !done(c, h) {
        c.poll(h)?;
        if Instant::now() > end {
            return Err(format!("timed out: ok {:?}, errors {:?}", h.ok, h.errors).into());
        }
    }
    Ok(())
}

/// Requests, a signed request and the busy limit, through either implementation.
fn exercise(lab: &Lab, cfg: &HttpClientConfig) -> R {
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| -> R {
        let srv = s.spawn(|| lab.serve_http(&stop, None).map_err(|e| e.to_string()));
        let result = (|| -> R {
            let target = lab.target("")?.with_header("X-MBX-APIKEY", "k");
            let mut c = HttpClient::new(cfg, target, lab.client_cfg.clone())?;
            assert_eq!(c.implementation(), cfg.implementation);
            let mut h = Seen::default();
            poll_until(&mut c, &mut h, |c, _| c.idle() == cfg.pool)?;
            let mut ids = Vec::new();
            for i in 0..cfg.pool {
                ids.push(c.request("GET", &format!("/r{i}"), &[], &[])?);
            }
            assert!(matches!(
                c.request("GET", "/x", &[], &[]),
                Err(Error::Busy(_))
            ));
            poll_until(&mut c, &mut h, |_, h| h.ok.len() == cfg.pool)?;
            for (i, id) in ids.iter().enumerate() {
                let got = h.ok.iter().find(|r| r.0 == *id).ok_or("missing response")?;
                assert_eq!((got.1, got.2.as_str()), (200, format!("/r{i}").as_str()));
            }
            let signer = Signer::hmac_sha256(b"secret");
            let sid = c.signed_request("POST", "/api/v3/order", "symbol=BTCUSDT", &[], &signer)?;
            poll_until(&mut c, &mut h, |_, h| h.ok.iter().any(|r| r.0 == sid))?;
            let body = &h.ok.iter().find(|r| r.0 == sid).ok_or("missing")?.2;
            assert!(
                body.starts_with("/api/v3/order?symbol=BTCUSDT&timestamp="),
                "{body}"
            );
            assert!(body.contains("&signature="));
            // HEAD: the length is announced, no body follows; both
            // implementations must complete it at once.
            let hid = c.request("HEAD", "/head", &[], &[])?;
            poll_until(&mut c, &mut h, |_, h| {
                h.ok.iter().any(|r| r.0 == hid) || !h.errors.is_empty()
            })?;
            let head = h.ok.iter().find(|r| r.0 == hid).ok_or("HEAD failed")?;
            assert_eq!((head.1, head.2.as_str()), (200, ""));
            assert!(h.errors.is_empty(), "{:?}", h.errors);
            Ok(())
        })();
        stop.store(true, Ordering::Relaxed);
        srv.join().map_err(|_| "server panicked")??;
        result
    })
}

#[test]
fn native_pool_requests_signs_and_limits() -> R {
    let lab = Lab::new()?;
    let mut cfg = HttpClientConfig::new(HttpImpl::Native);
    cfg.pool = 3;
    exercise(&lab, &cfg)
}

#[test]
fn native_pool_replaces_a_dropped_connection() -> R {
    let lab = Lab::new()?;
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| -> R {
        // The first connection closes after one response.
        let srv = s.spawn(|| lab.serve_http(&stop, Some(1)).map_err(|e| e.to_string()));
        let result = (|| -> R {
            let mut cfg = HttpClientConfig::new(HttpImpl::Native);
            cfg.pool = 1;
            let mut c = HttpClient::new(&cfg, lab.target("")?, lab.client_cfg.clone())?;
            let mut h = Seen::default();
            poll_until(&mut c, &mut h, |c, _| c.idle() == 1)?;
            c.request("GET", "/one", &[], &[])?;
            poll_until(&mut c, &mut h, |_, h| h.ok.len() == 1)?;
            // The server closed it; the pool reopens a connection by itself.
            poll_until(&mut c, &mut h, |c, _| c.idle() == 1)?;
            let id = c.request("GET", "/two", &[], &[])?;
            poll_until(&mut c, &mut h, |_, h| h.ok.iter().any(|r| r.0 == id))?;
            Ok(())
        })();
        stop.store(true, Ordering::Relaxed);
        srv.join().map_err(|_| "server panicked")??;
        result
    })
}

#[derive(Default)]
struct Counting {
    seen: Seen,
    connect_errors: usize,
}

impl ClientHandler for Counting {
    fn on_response(&mut self, request: u64, r: &Response<'_>) {
        self.seen.on_response(request, r);
    }
    fn on_error(&mut self, request: u64, error: Error) {
        self.seen.on_error(request, error);
    }
    fn on_connect_error(&mut self, _error: Error) {
        self.connect_errors += 1;
    }
}

#[test]
fn connect_failures_are_reported_not_returned() -> R {
    let lab = Lab::new()?;
    // TCP refuses a multicast destination inside connect() itself
    // (ENETUNREACH): every attempt fails synchronously, on the polling thread.
    let addr = std::net::SocketAddr::from(([224, 0, 0, 1], 443));
    let target = ergon_io::Target::new("localhost", 443, "").with_addr(addr);
    for background in [false, true] {
        let mut cfg = HttpClientConfig::new(HttpImpl::Native);
        cfg.pool = 2;
        cfg.background_connect = background;
        let mut c = HttpClient::new(&cfg, target.clone(), lab.client_cfg.clone())?;
        let mut h = Counting::default();
        let end = Instant::now() + Duration::from_secs(10);
        while h.connect_errors < 4 {
            c.poll(&mut h)?;
            assert!(
                Instant::now() < end,
                "background={background}: connect errors not reported"
            );
        }
        assert_eq!(c.idle(), 0);
    }
    Ok(())
}

#[test]
fn unhandled_connect_failures_are_counted_and_warned() -> R {
    // `Seen` does not implement on_connect_error: the default warns (stderr,
    // from a background thread, rate-limited) and the client counts them.
    let lab = Lab::new()?;
    let addr = std::net::SocketAddr::from(([224, 0, 0, 1], 443));
    let target = ergon_io::Target::new("localhost", 443, "").with_addr(addr);
    let mut cfg = HttpClientConfig::new(HttpImpl::Native);
    cfg.pool = 1;
    let mut c = HttpClient::new(&cfg, target, lab.client_cfg)?;
    let mut h = Seen::default();
    let end = Instant::now() + Duration::from_secs(10);
    while c.connect_failures() < 2 {
        c.poll(&mut h)?;
        assert!(Instant::now() < end, "connect failures not counted");
    }
    assert!(h.errors.is_empty(), "{:?}", h.errors);
    Ok(())
}

#[test]
fn curl_is_unsupported_without_the_feature_or_works_with_it() -> R {
    let lab = Lab::new()?;
    let mut cfg = HttpClientConfig::new(HttpImpl::Curl);
    cfg.pool = 2;
    cfg.curl_ca_file = Some(lab.ca_file()?.display().to_string());
    if cfg!(feature = "curl") {
        for drive in [
            ergon_io::CurlDrive::Perform,
            ergon_io::CurlDrive::SocketAction,
        ] {
            cfg.curl_drive = drive;
            let lab = Lab::new()?;
            cfg.curl_ca_file = Some(lab.ca_file()?.display().to_string());
            exercise(&lab, &cfg).map_err(|e| format!("{drive:?}: {e}"))?;
        }
    } else {
        let r = HttpClient::new(&cfg, lab.target("")?, lab.client_cfg.clone());
        assert!(matches!(r, Err(Error::Unsupported(_))));
    }
    Ok(())
}
