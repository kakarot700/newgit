//! Property-based tests for the core codecs and object layer.
//! Invariants, not examples (TEST_MATRIX I1/I4/I10).

use newgit::object::envelope;
use newgit::object::types::*;
use newgit::object::ObjectId;
use newgit::util::{base64, hex, varint};
use proptest::prelude::*;

fn arb_name() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9._-]{1,40}".prop_filter("no dot-only names", |s| {
        s != "." && s != ".." && !s.ends_with('.')
    })
}

fn arb_tree(max_entries: usize) -> impl Strategy<Value = Tree> {
    prop::collection::btree_map(arb_name(), (0..3u8, any::<[u8; 32]>()), 0..max_entries).prop_map(
        |m| Tree {
            entries: m
                .into_iter()
                .map(|(name, (mode, oid))| TreeEntry {
                    name,
                    mode: match mode {
                        0 => EntryMode::File,
                        1 => EntryMode::Executable,
                        2 => EntryMode::Symlink,
                        _ => EntryMode::Tree,
                    },
                    oid: ObjectId::from_bytes(oid),
                })
                .collect(),
        },
    )
}

proptest! {
    #[test]
    fn hex_roundtrip(b in prop::collection::vec(any::<u8>(), 0..300)) {
        prop_assert_eq!(hex::decode(&hex::encode(&b)).unwrap(), b);
    }

    #[test]
    fn varint_roundtrip(v in any::<u64>()) {
        let mut buf = Vec::new();
        varint::write_u64(&mut buf, v);
        let mut r = varint::Reader::new(&buf);
        prop_assert_eq!(r.read_u64().unwrap(), v);
        prop_assert!(r.is_empty());
    }

    #[test]
    fn svarint_roundtrip(v in any::<i64>()) {
        let mut buf = Vec::new();
        varint::write_i64(&mut buf, v);
        let mut r = varint::Reader::new(&buf);
        prop_assert_eq!(r.read_i64().unwrap(), v);
    }

    #[test]
    fn base64_roundtrip(b in prop::collection::vec(any::<u8>(), 0..300)) {
        prop_assert_eq!(base64::decode(&base64::encode(&b)).unwrap(), b);
    }

    #[test]
    fn blob_identity_and_roundtrip(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let o = Object::Blob(data.clone());
        let id = o.id();
        let d = Object::from_canonical(&o.canonical()).unwrap();
        let did = d.id();
        let o2 = Object::Blob(data);
        prop_assert_eq!(o2.id(), id);
        prop_assert_eq!(did, id);
        prop_assert_eq!(d, o); // deterministic identity (I1)
    }

    #[test]
    fn tree_roundtrip_and_sorted_invariant(t in arb_tree(32)) {
        let o = Object::Tree(t.clone());
        let canon = o.canonical();
        let id = o.id();
        let d = Object::from_canonical(&canon).unwrap();
        // canonical form of a re-sorted equal tree is byte-identical
        let mut t2 = t.clone();
        t2.entries.reverse();
        t2.entries.sort_by(|a, b| a.name.cmp(&b.name));
        let canon2 = Object::Tree(t2).canonical();
        let did = d.id();
        prop_assert_eq!(did, id);
        prop_assert_eq!(canon2, canon);
        prop_assert_eq!(d, o);
    }

    #[test]
    fn snapshot_roundtrip(
        parents in prop::collection::vec(any::<[u8;32]>(), 0..4),
        root in any::<[u8;32]>(),
        author in any::<[u8;32]>(),
        ts in -62135596800000i64..253402300799000i64,
        tz in -1440i64..=1440i64,
        msg in ".{0,200}",
        extras in prop::collection::btree_map("[a-z]{1,8}", "[a-z]{0,16}", 0..4),
    ) {
        let s = Snapshot {
            parents: parents.iter().map(|p| ObjectId::from_bytes(*p)).collect(),
            root: ObjectId::from_bytes(root),
            author: ObjectId::from_bytes(author),
            timestamp_ms: ts,
            tz_offset_min: tz as i16,
            message: msg.clone(),
            workspace: None,
            change: None,
            goal: None,
            extras: extras.clone(),
        };
        // dedupe parents (protocol requires unique)
        let mut seen = std::collections::HashSet::new();
        let s = Snapshot { parents: s.parents.into_iter().filter(|p| seen.insert(*p)).collect(), ..s };
        if s.validate().is_err() { return Ok(()); } // control chars etc.
        let o = Object::Snapshot(s.clone());
        let d = Object::from_canonical(&o.canonical()).unwrap();
        prop_assert_eq!(d, o);
    }

    #[test]
    fn from_canonical_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        // total decoder: any input either parses or errors (I4)
        let _ = Object::from_canonical(&bytes);
    }

    #[test]
    fn envelope_bitflip_detected(
        data in prop::collection::vec(any::<u8>(), 1..2048),
        pos_ratio in 0.0f64..1.0f64,
    ) {
        let obj = Object::Blob(data);
        let mut env = envelope::encode(&obj).unwrap();
        let pos = ((env.len() as f64) * pos_ratio) as usize % env.len();
        let bit = 1u8 << (pos % 8);
        env[pos] ^= bit;
        match envelope::decode(&env, 1 << 30) {
            Err(_) => {} // detected — good
            Ok((o, id)) => {
                // If it somehow decodes (flip in a non-verified padding bit
                // of the compressed stream is impossible for zlib; flips in
                // raw_len/stored_len change verification), the id must still
                // equal the recomputed identity — never a silent alias.
                prop_assert_eq!(id, o.id());
            }
        }
    }

    #[test]
    fn extras_map_invariants(m in prop::collection::btree_map("[a-z]{1,8}", "[a-z]{0,16}", 0..8)) {
        let a = Actor {
            kind: ActorKind::Agent,
            id: "agent:prop".into(),
            display_name: "P".into(),
            tool: "t".into(),
            tool_version: "v".into(),
            pubkey: None,
            extras: m.clone(),
        };
        let o = Object::Actor(a);
        let d = Object::from_canonical(&o.canonical()).unwrap();
        let d2 = d.clone();
        prop_assert_eq!(d, o);
        if let Object::Actor(a2) = d2 {
            prop_assert_eq!(a2.extras, m);
        }
    }
    // ── diff: opcodes reconstruct exactly and are deterministic ──
    #[test]
    fn diff_reconstructs_and_is_deterministic(
        va in prop::collection::vec("[abc]\n", 0..40),
        vb in prop::collection::vec("[abc]\n", 0..40),
    ) {
        let a: Vec<&[u8]> = va.iter().map(|s| s.as_bytes()).collect();
        let b: Vec<&[u8]> = vb.iter().map(|s| s.as_bytes()).collect();
        let ops1 = newgit::diff::myers::diff_lines(&a, &b, 1024);
        let ops2 = newgit::diff::myers::diff_lines(&a, &b, 1024);
        prop_assert_eq!(
            ops1.as_ref().map(|o| format!("{o:?}")),
            ops2.as_ref().map(|o| format!("{o:?}"))
        );
        if let Some(ops) = &ops1 {
            // ranges monotone and in-bounds; Equal ranges truly equal
            let mut alast = 0usize;
            let mut blast = 0usize;
            for op in ops {
                prop_assert!(op.a1 >= alast && op.b1 >= blast);
                prop_assert!(op.a2 <= a.len() && op.b2 <= b.len());
                alast = op.a2;
                blast = op.b2;
                if op.tag == newgit::diff::myers::Tag::Equal {
                    prop_assert_eq!(&a[op.a1..op.a2], &b[op.b1..op.b2]);
                }
            }
            prop_assert_eq!(alast, a.len());
            prop_assert_eq!(blast, b.len());
            // reconstruction invariant: applying opcodes to a yields b
            prop_assert_eq!(newgit::diff::myers::reconstruct(&a, &b, ops), b);
        }
    }
}
