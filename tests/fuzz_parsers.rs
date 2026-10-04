//! Fuzz-like parser suite (TEST_MATRIX "Fuzz-like", invariant I4):
//! seeded pseudo-random bytes — pure random and *prefix-anchored* (valid
//! magic followed by garbage, to reach deep parse paths) — against every
//! parser that ever touches untrusted input.
//!
//! Invariant: parsers never panic, never hang, never allocate unboundedly
//! (internal caps do the work); garbage returns Err. This is not coverage-
//! guided fuzzing (no external deps, D-002) — it is a deterministic,
//! reproducible garbage firehose that runs in CI-scale time.

use newgit::object::envelope;
use newgit::object::types::Object;
use newgit::object::ObjectId;
use newgit::repo::config::RepoConfig;
use newgit::repo::index::Index;
use newgit::repo::txn::Journal;
use newgit::util::base64;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// Random bytes, half the time anchored behind a valid-looking prefix.
    fn bytes(&mut self, prefix: Option<&[u8]>) -> Vec<u8> {
        let len = self.below(4096);
        let mut out = Vec::with_capacity(len + 16);
        if let Some(p) = prefix {
            if self.below(2) == 0 {
                out.extend_from_slice(p);
            }
        }
        while out.len() < len {
            out.extend_from_slice(&self.next().to_le_bytes());
        }
        out.truncate(len);
        out
    }
    fn string(&mut self, prefix: Option<&str>) -> String {
        let b = self.bytes(prefix.map(|p| p.as_bytes()));
        let mut s = String::from_utf8_lossy(&b).into_owned();
        // lossy conversion strips NULs rarely; also inject raw text shapes
        if self.below(4) == 0 {
            s.push_str(&format!(
                " {} = {} \" \\ \n\t \u{7f} 18446744073709551616 -1 0xdeadbeef",
                self.next(),
                self.next()
            ));
        }
        s
    }
}

const ITERS: usize = 20_000;

#[test]
fn fuzz_envelope_decode_never_panics() {
    let mut rng = Rng(0xF00F_0001);
    for _ in 0..ITERS {
        let b = rng.bytes(Some(b"NGOB"));
        let _ = envelope::decode(&b, 1 << 20);
        // also with a tiny cap: limit paths must reject, not allocate
        let _ = envelope::decode(&b, 8);
    }
}

#[test]
fn fuzz_object_canonical_never_panics() {
    let mut rng = Rng(0xF00F_0002);
    for _ in 0..ITERS {
        // type tags are 1..=9 — anchor half the inputs behind each tag
        let tag: u8 = (rng.below(9) + 1) as u8;
        let mut b = vec![tag];
        b.extend_from_slice(&rng.bytes(None));
        let _ = Object::from_canonical(&b);
        let _ = ObjectId::compute(&b);
    }
}

#[test]
fn fuzz_index_decode_never_panics() {
    let mut rng = Rng(0xF00F_0003);
    for _ in 0..ITERS {
        let b = rng.bytes(Some(b"NGIX\x01"));
        let _ = Index::decode(&b);
    }
}

#[test]
fn fuzz_journal_parse_never_panics() {
    let mut rng = Rng(0xF00F_0004);
    for _ in 0..(ITERS / 2) {
        let s = rng.string(Some("NEWGIT-TXN v1\nstate=RUNNING\n"));
        let _ = Journal::parse(&s);
    }
}

#[test]
fn fuzz_config_parse_never_panics() {
    let mut rng = Rng(0xF00F_0005);
    for _ in 0..(ITERS / 2) {
        let s = rng.string(Some("format_version = 1\n"));
        let _ = RepoConfig::parse(&s);
    }
}

#[test]
fn fuzz_hex_base64_refnames_never_panic() {
    let mut rng = Rng(0xF00F_0006);
    for _ in 0..ITERS {
        let s = rng.string(None);
        let _ = ObjectId::from_hex(&s);
        let _ = ObjectId::from_hex(s.trim());
        let _ = base64::decode(&s);
        let _ = newgit::repo::refs::check_ref_name(&s);
        let _ = newgit::repo::refs::check_ref_name_system(&s);
        let _ = newgit::util::fsx::check_rel_path(&s, 255);
    }
}
