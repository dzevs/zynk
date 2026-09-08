//! Tracked regression set for the Darwin static-archive parser in `build.rs` (Gate-3 SENT-R6-BUILD-001; Codex
//! pre-tag note): the parser that decides whether Apple's `libtool -static` rewrite is needed, and that verifies
//! the rewritten archive, must accept only structurally complete `ar` input — plain and BSD `#1/N` names, odd
//! padding, the `__.SYMDEF` table — and reject trailing partial headers, payloads beyond EOF, and BSD name lengths
//! that exceed the member size.

#[path = "../build.rs"]
#[allow(dead_code, unused_imports)]
mod build_script;

use build_script::archive_misaligned_members;

const MAGIC: &[u8] = b"!<arch>\n";

fn header(name_field: &str, size: usize) -> Vec<u8> {
    let mut h = format!(
        "{name_field:<16}{:<12}{:<6}{:<6}{:<8}{size:<10}",
        0, 0, 0, "644"
    )
    .into_bytes();
    h.extend_from_slice(b"`\n");
    assert_eq!(h.len(), 60);
    h
}

/// A BSD-style member: `#1/<len>` name field, the name stored in front of the payload and counted in the size.
fn bsd_member(name: &str, payload: &[u8]) -> Vec<u8> {
    let size = name.len() + payload.len();
    let mut m = header(&format!("#1/{}", name.len()), size);
    m.extend_from_slice(name.as_bytes());
    m.extend_from_slice(payload);
    if size % 2 == 1 {
        m.push(b'\n');
    }
    m
}

/// A plain-name member (name in the 16-byte field).
fn plain_member(name: &str, payload: &[u8]) -> Vec<u8> {
    let mut m = header(name, payload.len());
    m.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        m.push(b'\n');
    }
    m
}

fn archive(members: &[Vec<u8>]) -> Vec<u8> {
    let mut a = MAGIC.to_vec();
    for m in members {
        a.extend_from_slice(m);
    }
    a
}

#[test]
fn bsd_names_are_read_and_alignment_is_judged_at_the_payload() {
    // First member: 4-byte name + 4-byte payload -> payload at 8 + 60 + 4 = 72 (aligned), next header at 76.
    let first = bsd_member("ab.o", &[1u8; 4]);
    let second = bsd_member("compiler_rt.o", &[2u8; 16]); // payload at 76 + 60 + 13 = 149 -> misaligned
    let result =
        archive_misaligned_members(&archive(&[first, second])).expect("well-formed archive");
    assert_eq!(result, vec!["compiler_rt.o".to_string()]);
}

#[test]
fn symdef_is_never_reported_and_plain_names_work() {
    let symdef = plain_member("__.SYMDEF", &[0u8; 20]); // payload at 68 -> misaligned but exempt
    let obj = plain_member("main.o", &[0u8; 8]); // header at 88, payload at 148 -> misaligned
    let result = archive_misaligned_members(&archive(&[symdef, obj])).expect("well-formed archive");
    assert_eq!(result, vec!["main.o".to_string()]);
}

#[test]
fn an_aligned_archive_reports_nothing() {
    let a = plain_member("a.o", &[0u8; 20]); // payload at 68 -> misaligned
    let result =
        archive_misaligned_members(&archive(std::slice::from_ref(&a))).expect("well-formed");
    assert_eq!(result, vec!["a.o".to_string()]);
    // A 16-byte payload after a 4-byte-shifted name puts the next payload at 8+60+16+60 = 144 -> aligned.
    let b = plain_member("b.o", &[0u8; 16]);
    let result = archive_misaligned_members(&archive(&[plain_member("x", &[0u8; 16]), b]))
        .expect("well-formed");
    assert_eq!(result, vec!["x".to_string()]);
}

#[test]
fn odd_payloads_are_padded_and_a_final_member_may_end_exactly_at_eof() {
    let odd = plain_member("odd.o", &[7u8; 5]); // padded
    assert!(
        archive_misaligned_members(&archive(&[odd.clone(), plain_member("z.o", &[0u8; 2])]))
            .is_ok()
    );
    let mut unpadded = archive(&[odd]);
    unpadded.pop(); // drop the trailing pad byte: EOF exactly at the payload end is still complete
    assert!(archive_misaligned_members(&unpadded).is_ok());
}

#[test]
fn trailing_partial_header_is_rejected() {
    let mut a = archive(&[plain_member("a.o", &[0u8; 8])]);
    a.extend_from_slice(b"garbage-after-the-last-member");
    let err = archive_misaligned_members(&a).expect_err("partial trailing header");
    assert!(err.contains("trailing"), "{err}");
}

#[test]
fn payload_beyond_eof_is_rejected() {
    let mut a = archive(&[plain_member("a.o", &[0u8; 8])]);
    a.truncate(8 + 60 + 4); // header claims 8 bytes, only 4 remain
    let err = archive_misaligned_members(&a).expect_err("payload beyond EOF");
    assert!(err.contains("beyond"), "{err}");
}

#[test]
fn bsd_name_length_larger_than_the_member_is_rejected() {
    let mut m = header("#1/40", 8); // name claims 40 bytes inside an 8-byte member
    m.extend_from_slice(&[0u8; 8]);
    let err = archive_misaligned_members(&archive(&[m])).expect_err("name exceeds member");
    assert!(err.contains("name length"), "{err}");
}

#[test]
fn bad_magic_and_bad_member_magic_are_rejected() {
    assert!(archive_misaligned_members(b"not an archive").is_err());
    let mut m = plain_member("a.o", &[0u8; 8]);
    m[58] = b'x';
    assert!(archive_misaligned_members(&archive(&[m])).is_err());
}
