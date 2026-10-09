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
