//! What a source attestation has to look like (ADR 0013 custody).
//!
//! `build.rs` compiles this file as a module of the build script and the crate compiles it as a
//! module of the binary, so the build INPUT and the custody boundary cannot drift apart about what
//! counts as an attestation. `ZYNK_BUILD_SHA` is a build input its caller chooses freely, so it is
//! checked where it enters the build and again where it authorises a copy to another host.

/// The length of a full git object name, the only shape custody accepts.
const FULL_OBJECT_NAME_LEN: usize = 40;

/// Why `value` cannot serve as a source attestation, or `None` when it can.
///
/// Custody needs the exact reviewed commit: a full 40-character lowercase git object name and
/// nothing else. An abbreviated or uppercase name is not the canonical form, a non-hex string names
/// no commit at all, and the `-dirty` suffix `build.rs` appends says the compiled tree was NOT the
/// commit it names — none of the three can authorise running those bytes on another host.
pub fn attested_sha_problem(value: &str) -> Option<String> {
    if value.is_empty() {
        return Some("it is empty, so it names no commit".to_string());
    }
    if let Some(commit) = value.strip_suffix("-dirty") {
        return Some(format!(
            "it is marked -dirty, so the compiled tree was not commit {commit}"
        ));
    }
    if value.len() != FULL_OBJECT_NAME_LEN {
        return Some(format!(
            "it is {} characters, not a full {FULL_OBJECT_NAME_LEN}-character git object name",
            value.len()
        ));
    }
    if !value.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f')) {
        return Some(format!(
            "it is not {FULL_OBJECT_NAME_LEN} lowercase hexadecimal characters"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::attested_sha_problem;

    #[test]
    fn a_full_lowercase_object_name_is_the_only_accepted_attestation() {
        assert_eq!(attested_sha_problem(&"a".repeat(40)), None);
        assert_eq!(
            attested_sha_problem("0123456789abcdef0123456789abcdef01234567"),
            None
        );
    }

    #[test]
    fn an_invalid_zynk_build_sha_input_fails_the_build() {
        // build.rs validates its ZYNK_BUILD_SHA input with exactly this predicate and panics when
        // it reports a problem, so an unusable attestation can never be compiled in at all.
        for value in [
            "",
            "abc123",
            "deadbeefcafe",
            &"A".repeat(40),
            &"a".repeat(39),
            &"a".repeat(41),
            "not-a-sha-at-all-not-a-sha-at-all-not-as",
            &format!("{}-dirty", "a".repeat(40)),
        ] {
            assert!(
                attested_sha_problem(value).is_some(),
                "{value:?} must not be accepted as a source attestation"
            );
        }
    }

    #[test]
    fn a_dirty_attestation_is_refused_by_name() {
        let problem = attested_sha_problem(&format!("{}-dirty", "b".repeat(40)))
            .expect("a dirty attestation must be refused");
        assert!(problem.contains("-dirty"), "unexpected problem: {problem}");
    }
}
