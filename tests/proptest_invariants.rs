//! Property-based invariants for adopt types.
//! READ-ONLY: the edit-agent must not modify this file.

use proptest::prelude::*;

// Verdict serialization round-trip.
proptest! {
    #[test]
    fn verdict_serde_roundtrip(idx in 0usize..4) {
        let verdicts = [
            adopt::types::Verdict::NotInstalled,
            adopt::types::Verdict::InstalledStale,
            adopt::types::Verdict::InstalledCurrent,
            adopt::types::Verdict::NotABin,
        ];
        let v = verdicts[idx].clone();
        let json = serde_json::to_string(&v).expect("serialize");
        let v2: adopt::types::Verdict = serde_json::from_str(&json).expect("deserialize");
        prop_assert_eq!(v, v2);
    }
}
