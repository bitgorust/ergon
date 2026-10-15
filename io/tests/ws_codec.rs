//! WebSocket frame codec.
#![allow(
    missing_docs,
    clippy::cast_possible_truncation,
    clippy::unnecessary_wraps
)]

type R = Result<(), Box<dyn std::error::Error>>;

use ergon_io::ws::{self, OP_BINARY, OP_CLOSE, OP_CONT, OP_PING, OP_TEXT};

#[test]
fn every_length_encoding_round_trips() -> R {
    for &len in &[0usize, 1, 125, 126, 127, 1000, 65_535, 65_536, 200_000] {
        for mask in [None, Some([1, 2, 3, 4])] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let mut out = vec![0u8; ws::header_len(len, mask.is_some()) + len];
            let n = ws::encode_frame(OP_BINARY, &payload, mask, &mut out)?;
            assert_eq!(n, out.len());
            let h = ws::parse_header(&out)?.ok_or("incomplete header")?;
            assert!(h.fin);
            assert_eq!(h.opcode, OP_BINARY);
            assert_eq!(h.payload_len, len);
            assert_eq!(h.mask, mask);
            let mut body = out[h.header_len..h.frame_len()].to_vec();
            if let Some(m) = h.mask {
                ws::apply_mask(&mut body, m);
            }
            assert_eq!(body, payload, "len {len} mask {mask:?}");
            // Every strict prefix of the header is "need more".
            for cut in 0..h.header_len {
                assert_eq!(ws::parse_header(&out[..cut])?, None, "len {len} cut {cut}");
            }
        }
    }
    Ok(())
}

#[test]
fn rejects_protocol_violations() -> R {
    assert!(
        ws::parse_header(&[0x80 | 0x40 | OP_TEXT, 0]).is_err(),
        "RSV1 without extension"
    );
    assert!(ws::parse_header(&[0x83, 0]).is_err(), "reserved opcode");
    assert!(
        ws::parse_header(&[OP_PING, 0]).is_err(),
        "fragmented control frame"
    );
    assert!(
        ws::parse_header(&[0x80 | OP_CLOSE, 126, 0, 200]).is_err(),
        "control frame over 125 bytes"
    );
    let mut big = [0u8; 10];
    big[0] = 0x80 | OP_BINARY;
    big[1] = 127;
    big[2] = 0x80;
    assert!(
        ws::parse_header(&big).is_err(),
        "64-bit length with top bit set"
    );
    Ok(())
}

#[test]
fn continuation_frames_parse() -> R {
    let h = ws::parse_header(&[OP_TEXT, 2, b'a', b'b'])?.ok_or("incomplete header")?;
    assert!(!h.fin);
    let h = ws::parse_header(&[0x80 | OP_CONT, 2, b'c', b'd'])?.ok_or("incomplete header")?;
    assert!(h.fin);
    assert_eq!(h.opcode, OP_CONT);
    Ok(())
}

#[test]
fn mask_matches_bytewise_xor() -> R {
    let mask = [0xAA, 0x55, 0x0F, 0xF0];
    for len in 0u8..20 {
        let orig: Vec<u8> = (0..len).collect();
        let mut fast = orig.clone();
        ws::apply_mask(&mut fast, mask);
        let slow: Vec<u8> = orig
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4])
            .collect();
        assert_eq!(fast, slow);
    }
    Ok(())
}

#[test]
fn handshake_accept_key_matches_rfc_example() -> R {
    // RFC 6455 section 1.3.
    assert_eq!(
        ws::accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
    Ok(())
}

#[test]
fn client_response_checks() -> R {
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let ok = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\nXY";
    assert_eq!(ws::parse_client_response(ok, key)?, Some(ok.len() - 2));
    assert_eq!(ws::parse_client_response(&ok[..20], key)?, None);
    let wrong = b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: nope\r\n\r\n";
    assert!(ws::parse_client_response(wrong, key).is_err());
    let deflate = b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n";
    assert!(ws::parse_client_response(deflate, key).is_err());
    let forbidden = b"HTTP/1.1 451 Unavailable\r\nContent-Length: 0\r\n\r\n";
    assert!(ws::parse_client_response(forbidden, key).is_err());
    Ok(())
}

#[test]
fn upgrade_requests_reject_injected_or_reserved_fields() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = ergon_io::tls::client_config(rustls::RootCertStore::empty())?;
    let target = |path: &str| ergon_io::Target::new("stream.example.test", 443, path);
    for (t, why) in [
        (
            target("/ws").with_header("X-Key", "v\r\nHost: evil"),
            "CR/LF in a value",
        ),
        (
            target("/ws").with_header("Bad Name", "1"),
            "space in a name",
        ),
        (
            target("/ws").with_header("Sec-WebSocket-Key", "x"),
            "a handshake field",
        ),
        (
            target("/ws").with_header("Upgrade", "h2c"),
            "a reserved field",
        ),
        (target("/a b"), "space in the path"),
        (target("/a\r\nX: 1"), "CR/LF in the path"),
    ] {
        assert!(
            ergon_io::WsDial::new(t, cfg.clone(), 1 << 20).is_err(),
            "accepted {why}"
        );
    }
    assert!(ergon_io::WsDial::new(target("/ws?x=1"), cfg, 1 << 20).is_ok());
    Ok(())
}

#[test]
fn upgrade_request_sends_only_the_callers_user_agent() {
    let none = String::from_utf8_lossy(&ws::client_request("h", "/", "k", &[])).to_lowercase();
    assert!(!none.contains("user-agent"), "{none}");
    let one = String::from_utf8_lossy(&ws::client_request("h", "/", "k", &[("User-Agent", "me")]))
        .to_lowercase();
    assert_eq!(one.matches("user-agent").count(), 1, "{one}");
}
