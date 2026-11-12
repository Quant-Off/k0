use super::*;

const EMPTY_KAT: [u64; 10] = [
    0x729a_8df3_30f9_87be,
    0xf967_9c17_f56b_d421,
    0x7818_2360_5f29_f7dd,
    0x7e60_bdc4_094a_542f,
    0x84e3_8a22_bc72_814f,
    0x91e0_93e2_aa90_40f8,
    0x23a0_eb5c_8e9a_62f4,
    0x3f5d_79bc_5cda_f469,
    0xbbe8_714f_ac11_7bc3,
    0x0456_b946_f38f_631b,
];

const VIRT_KAT: [u64; 10] = [
    0x58f5_c303_6590_4bec,
    0x6714_cdf3_e92d_e4f7,
    0x8ba0_a935_b534_e3f3,
    0xafb3_5b34_3801_40d0,
    0xc1cd_2500_e171_a98c,
    0xc7b2_4c0d_8e9a_7664,
    0x4d57_df02_cf8e_42fb,
    0x583a_a420_1a5c_76c7,
    0x98e1_cb7e_da9d_72ca,
    0x15ca_a266_234d_adaf,
];

fn virt_seed() -> Vec<u8> {
    (0u8..40).collect()
}

#[test]
fn derivation_matches_independent_known_answers() {
    assert_eq!(derive_pac_keys(&[], &[]), EMPTY_KAT);
    assert_eq!(derive_pac_keys(&virt_seed(), &[0x0123_4567, 0]), VIRT_KAT);
}

#[test]
fn every_input_bit_changes_the_keys() {
    let base = derive_pac_keys(&virt_seed(), &[0x0123_4567, 0]);
    for byte in 0..40 {
        let mut seed = virt_seed();
        seed[byte] ^= 1 << (byte % 8);
        assert_ne!(derive_pac_keys(&seed, &[0x0123_4567, 0]), base, "dtb byte {byte}");
    }
    for bit in 0..64 {
        assert_ne!(derive_pac_keys(&virt_seed(), &[0x0123_4567 ^ (1 << bit), 0]), base, "cntpct bit {bit}");
        assert_ne!(derive_pac_keys(&virt_seed(), &[0x0123_4567, 1 << bit]), base, "rndr bit {bit}");
    }
}

#[test]
fn length_prefix_separates_dtb_bytes_from_extra_words() {
    let w: u64 = 0x1122_3344_5566_7788;
    let mut joined = virt_seed();
    joined.extend_from_slice(&w.to_le_bytes());
    assert_ne!(derive_pac_keys(&joined, &[]), derive_pac_keys(&virt_seed(), &[w]));
    assert_ne!(derive_pac_keys(&[], &[0]), derive_pac_keys(&[0; 8], &[]));
}

#[test]
fn key_words_are_distinct() {
    for keys in [EMPTY_KAT, VIRT_KAT, derive_pac_keys(&[0xFF; 72], &[u64::MAX, u64::MAX])] {
        for (i, a) in keys.iter().enumerate() {
            assert!(keys[i + 1..].iter().all(|b| a != b));
            assert_ne!(*a, 0);
        }
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn root_seed_matches_independent_known_answers() {
    // Python hashlib로 따로 계산한 값
    assert_eq!(
        derive_root_seed(&[], &[]).to_vec(),
        hex("1fd07e587112efbf09cabed0587f70aab66fc7b863becf6e10a78021986bef4e")
    );
    assert_eq!(
        derive_root_seed(&virt_seed(), &[0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210]).to_vec(),
        hex("d0b6a805e0d9a276642dc45946a20ba8c3abb62bf37055395d038ac812855d55")
    );
}

#[test]
fn root_seed_is_independent_of_the_pac_keys_and_follows_every_input() {
    let seed = virt_seed();
    let base = derive_root_seed(&seed, &[7, 9]);
    // 같은 재료에서 나온 PAC 키의 어느 워드도 시드에 나타나지 않음
    for k in derive_pac_keys(&seed, &[7, 9]) {
        assert!(!base.windows(8).any(|w| w == k.to_le_bytes()));
    }
    let mut flipped = seed.clone();
    flipped[39] ^= 1;
    assert_ne!(derive_root_seed(&flipped, &[7, 9]), base);
    assert_ne!(derive_root_seed(&seed, &[7, 8]), base);
    assert_ne!(derive_root_seed(&seed[..39], &[7, 9]), base);
    // 길이 접두사가 DTB 바이트와 뒤따르는 워드를 구분함
    let tail = u64::from_le_bytes(seed[32..40].try_into().unwrap());
    assert_ne!(derive_root_seed(&seed[..32], &[tail, 7, 9]), base);
}
