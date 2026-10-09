use super::*;

const SEG_ALIGN: u64 = 16 * 1024;

#[test]
fn embedded_image_passes_its_own_integrity_check() {
    let rt = verify_root_task().expect("embedded root task must verify");
    assert_eq!(rt.image.len() as u64 % SEG_ALIGN, 0);
    assert!(!rt.image.is_empty());
    assert_eq!(rt.base % SEG_ALIGN, 0);
}

#[test]
fn segments_follow_the_layout_contract() {
    let rt = verify_root_task().unwrap();
    let image_end = rt.base + rt.image.len() as u64;
    assert!(!rt.segments.is_empty() && rt.segments.len() <= 4);
    let mut prev_end = rt.base;
    for s in rt.segments {
        assert_eq!(s.va % SEG_ALIGN, 0, "{s:?}");
        assert!(s.memsz > 0, "{s:?}");
        assert!(s.va >= prev_end, "{s:?} overlaps the previous segment");
        prev_end = s.va + s.memsz;
        assert!(prev_end <= image_end, "{s:?} leaves the flat image");
    }
}

#[test]
fn entry_point_lies_in_the_text_segment() {
    let rt = verify_root_task().unwrap();
    let holder: Vec<&RtSegment> = rt
        .segments
        .iter()
        .filter(|s| (s.va..s.va + s.memsz).contains(&rt.entry))
        .collect();
    assert_eq!(holder.len(), 1);
    assert_eq!(holder[0].kind, RtSegKind::Text);
    assert_eq!(rt.entry % 4, 0);
}

#[test]
fn integrity_hash_is_recomputed_not_copied() {
    let rt = verify_root_task().unwrap();
    let again: [u8; 32] = Sha256::digest(rt.image).into();
    assert_eq!(rt.sha256, again);
    let mut tampered = rt.image.to_vec();
    tampered[rt.image.len() / 2] ^= 1;
    let changed: [u8; 32] = Sha256::digest(&tampered).into();
    assert_ne!(changed, rt.sha256);
}
