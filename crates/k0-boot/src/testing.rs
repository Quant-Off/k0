//! 테스트 지원 모듈입니다.
//!
//! # Features
//! 이 크레이트의 테스트와, feature `test-support`를 켠 다른 크레이트의 테스트가
//! 함께 쓰는 도구입니다. DTB를 조립하는 빌더와 트리 도우미, 실제 보드와 같은
//! 모양의 트리, 결정적 난수와 변이 함수가 들어 있습니다. 커널 빌드에는
//! 포함되지 않습니다.

pub use crate::fdt::builder;

use crate::fdt::builder::{set_be32, Builder, OFF_TOTALSIZE};
use crate::MAX_DTB_SIZE;

/// 시드 하나로 재현되는 난수 생성기입니다.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(splitmix(seed) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    pub fn index(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

pub fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

pub fn env_u64(name: &str) -> Option<u64> {
    let v = std::env::var(name).ok()?;
    let v = v.trim().replace('_', "");
    match v.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => v.parse().ok(),
    }
}

pub fn interesting(rng: &mut Rng, len: usize) -> u32 {
    let len = len as u32;
    let pool = [
        0,
        1,
        2,
        3,
        4,
        8,
        9,
        16,
        17,
        32,
        33,
        40,
        0x7F,
        0x80,
        0xFF,
        0xFFFF,
        0x7FFF_FFFF,
        0x8000_0000,
        u32::MAX,
        u32::MAX - 3,
        len,
        len.wrapping_sub(4),
        len.wrapping_add(4),
        0xd00d_feed,
        MAX_DTB_SIZE,
        MAX_DTB_SIZE + 1,
    ];
    if rng.below(4) == 0 { rng.next() as u32 } else { pool[rng.index(pool.len())] }
}

pub fn mutate(rng: &mut Rng, blob: &mut Vec<u8>) {
    for _ in 0..1 + rng.below(4) {
        let len = blob.len();
        if len < 8 {
            blob.extend(rng.bytes(8));
            continue;
        }
        match rng.below(10) {
            0 => {
                let i = rng.index(len);
                blob[i] ^= 1 << rng.below(8);
            }
            1 => {
                let i = rng.index(len);
                blob[i] = [0x00, 0x01, 0x7F, 0x80, 0xFF][rng.index(5)];
            }
            2 => {
                let i = rng.index(len / 4) * 4;
                let v = interesting(rng, len);
                set_be32(blob, i, v);
            }
            3 if len >= 40 => {
                let field = rng.index(10) * 4;
                let v = interesting(rng, len);
                set_be32(blob, field, v);
            }
            4 => blob.truncate(rng.index(len)),
            5 => {
                let n = rng.index(64);
                blob.extend(rng.bytes(n));
            }
            6 => {
                let a = rng.index(len);
                let b = rng.index(len);
                let n = rng.index((len - a.max(b)).min(64) + 1);
                blob.copy_within(a..a + n, b);
            }
            7 => {
                let i = rng.index(len / 4) * 4;
                let v = interesting(rng, len);
                blob.splice(i..i, v.to_be_bytes());
            }
            8 => {
                let i = rng.index(len - 3);
                blob.drain(i..i + 4);
            }
            _ if len >= 8 => {
                set_be32(blob, OFF_TOTALSIZE, len as u32);
            }
            _ => {}
        }
    }
}

/// 테스트용 트리 노드입니다. 프로퍼티를 먼저, 자식을 뒤에 씁니다.
#[derive(Clone, Debug)]
pub struct N {
    pub name: String,
    pub props: Vec<(String, Vec<u8>)>,
    pub kids: Vec<N>,
}

impl N {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            props: Vec::new(),
            kids: Vec::new(),
        }
    }

    pub fn p(mut self, key: &str, value: Vec<u8>) -> Self {
        self.set(key, value);
        self
    }

    pub fn c(mut self, kid: N) -> Self {
        self.kids.push(kid);
        self
    }

    pub fn set(&mut self, key: &str, value: Vec<u8>) {
        self.del(key);
        self.props.push((key.to_string(), value));
    }

    pub fn del(&mut self, key: &str) {
        self.props.retain(|(k, _)| k != key);
    }

    pub fn kid(&mut self, name: &str) -> &mut N {
        self.kids
            .iter_mut()
            .find(|k| k.name == name)
            .unwrap_or_else(|| panic!("no child {name}"))
    }

    pub fn write(&self, b: &mut Builder) {
        b.begin(&self.name);
        for (k, v) in &self.props {
            b.prop(k, v);
        }
        for kid in &self.kids {
            kid.write(b);
        }
        b.end();
    }

    pub fn blob(&self) -> Vec<u8> {
        let mut b = Builder::new();
        self.write(&mut b);
        b.build()
    }
}

pub fn cells(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

pub fn strs(v: &[&str]) -> Vec<u8> {
    v.iter().flat_map(|s| s.bytes().chain([0])).collect()
}

pub fn flag() -> Vec<u8> {
    Vec::new()
}

pub const GIC_PHANDLE: u32 = 0x8002;

pub fn memory() -> N {
    N::new("memory@40000000")
        .p("device_type", strs(&["memory"]))
        .p("reg", cells(&[0, 0x4000_0000, 0, 0x2000_0000]))
}

pub fn timer(flags: u32) -> N {
    N::new("timer")
        .p("compatible", strs(&["arm,armv8-timer", "arm,armv7-timer"]))
        .p(
            "interrupts",
            cells(&[1, 13, flags, 1, 14, flags, 1, 11, flags, 1, 10, flags]),
        )
        .p("always-on", flag())
}

pub fn pl011() -> N {
    N::new("pl011@9000000")
        .p("compatible", strs(&["arm,pl011", "arm,primecell"]))
        .p("reg", cells(&[0, 0x900_0000, 0, 0x1000]))
}

pub fn gicv3() -> N {
    N::new("intc@8000000")
        .p("compatible", strs(&["arm,gic-v3"]))
        .p("#interrupt-cells", cells(&[3]))
        .p("interrupt-controller", flag())
        .p("phandle", cells(&[GIC_PHANDLE]))
        .p("#redistributor-regions", cells(&[1]))
        .p("reg", cells(&[0, 0x800_0000, 0, 0x1_0000, 0, 0x80a_0000, 0, 0xf6_0000]))
        .p("#address-cells", cells(&[2]))
        .p("#size-cells", cells(&[2]))
        .p("ranges", flag())
        .c(N::new("its@8080000").p("compatible", strs(&["arm,gic-v3-its"])))
}

pub fn gicv2() -> N {
    N::new("intc@8000000")
        .p("compatible", strs(&["arm,cortex-a15-gic"]))
        .p("#interrupt-cells", cells(&[3]))
        .p("interrupt-controller", flag())
        .p("phandle", cells(&[GIC_PHANDLE]))
        .p(
            "reg",
            cells(&[
                0, 0x800_0000, 0, 0x1_0000, 0, 0x801_0000, 0, 0x1_0000, 0, 0x803_0000, 0, 0x1_0000,
                0, 0x804_0000, 0, 0x1_0000,
            ]),
        )
}

/// QEMU virt가 만드는 DTB와 같은 모양의 트리입니다.
pub fn virt(gic: N, timer_flags: u32) -> N {
    N::new("")
        .p("#address-cells", cells(&[2]))
        .p("#size-cells", cells(&[2]))
        .p("interrupt-parent", cells(&[GIC_PHANDLE]))
        .p("compatible", strs(&["linux,dummy-virt"]))
        .c(N::new("chosen").p("stdout-path", strs(&["/pl011@9000000"])))
        .c(memory())
        .c(pl011())
        .c(gic)
        .c(timer(timer_flags))
}

pub fn virt3() -> N {
    virt(gicv3(), 4)
}

pub fn virt2() -> N {
    virt(gicv2(), 0x104)
}

/// Raspberry Pi 4(bcm2711)와 같은 모양의 트리입니다. 디바이스가 /soc 아래에
/// 있고 ranges 항목이 셋이며 콘솔은 별칭으로 가리킵니다.
pub fn rpi4() -> N {
    let soc = N::new("soc")
        .p("compatible", strs(&["simple-bus"]))
        .p("#address-cells", cells(&[1]))
        .p("#size-cells", cells(&[1]))
        .p(
            "ranges",
            cells(&[
                0x7e00_0000, 0, 0xfe00_0000, 0x180_0000, 0x7c00_0000, 0, 0xfc00_0000, 0x200_0000,
                0x4000_0000, 0, 0xff80_0000, 0x80_0000,
            ]),
        )
        .c(N::new("serial@7e201000")
            .p("compatible", strs(&["arm,pl011", "arm,primecell"]))
            .p("reg", cells(&[0x7e20_1000, 0x200])))
        .c(N::new("serial@7e215040")
            .p("compatible", strs(&["brcm,bcm2835-aux-uart"]))
            .p("reg", cells(&[0x7e21_5040, 0x40])))
        .c(N::new("interrupt-controller@40041000")
            .p("compatible", strs(&["arm,gic-400"]))
            .p("#interrupt-cells", cells(&[3]))
            .p("interrupt-controller", flag())
            .p("phandle", cells(&[1]))
            .p(
                "reg",
                cells(&[
                    0x4004_1000, 0x1000, 0x4004_2000, 0x2000, 0x4004_4000, 0x2000, 0x4004_6000, 0x2000,
                ]),
            ));
    N::new("")
        .p("#address-cells", cells(&[2]))
        .p("#size-cells", cells(&[1]))
        .p("interrupt-parent", cells(&[1]))
        .c(N::new("aliases")
            .p("serial0", strs(&["/soc/serial@7e201000"]))
            .p("serial1", strs(&["/soc/serial@7e215040"])))
        .c(N::new("chosen").p("stdout-path", strs(&["serial1:115200n8"])))
        .c(N::new("memory@0")
            .p("device_type", strs(&["memory"]))
            .p("reg", cells(&[0, 0, 0x3b40_0000])))
        .c(soc)
        .c(N::new("timer")
            .p("compatible", strs(&["arm,armv8-timer"]))
            .p(
                "interrupts",
                cells(&[1, 13, 0xf08, 1, 14, 0xf08, 1, 11, 0xf08, 1, 10, 0xf08]),
            ))
}

/// RK3399와 같은 모양의 트리입니다. 인터럽트 지정자가 네 셀이고 콘솔은
/// 레지스터 간격 4바이트의 DesignWare UART입니다.
pub fn rk3399() -> N {
    N::new("")
        .p("#address-cells", cells(&[2]))
        .p("#size-cells", cells(&[2]))
        .p("interrupt-parent", cells(&[7]))
        .c(N::new("aliases").p("serial2", strs(&["/serial@ff1a0000"])))
        .c(N::new("chosen").p("stdout-path", strs(&["serial2:1500000n8"])))
        .c(N::new("memory@200000")
            .p("device_type", strs(&["memory"]))
            .p("reg", cells(&[0, 0x20_0000, 0, 0x7fe0_0000])))
        .c(N::new("serial@ff1a0000")
            .p("compatible", strs(&["rockchip,rk3399-uart", "snps,dw-apb-uart"]))
            .p("reg", cells(&[0, 0xff1a_0000, 0, 0x100]))
            .p("reg-shift", cells(&[2]))
            .p("reg-io-width", cells(&[4])))
        .c(N::new("interrupt-controller@fee00000")
            .p("compatible", strs(&["arm,gic-v3"]))
            .p("#interrupt-cells", cells(&[4]))
            .p("interrupt-controller", flag())
            .p("phandle", cells(&[7]))
            .p(
                "reg",
                cells(&[
                    0, 0xfee0_0000, 0, 0x1_0000, 0, 0xfef0_0000, 0, 0xc_0000, 0, 0xfff0_0000, 0, 0x1_0000,
                ]),
            ))
        .c(N::new("timer")
            .p("compatible", strs(&["arm,armv8-timer"]))
            .p(
                "interrupts",
                cells(&[1, 13, 8, 0, 1, 14, 8, 0, 1, 11, 8, 0, 1, 10, 8, 0]),
            ))
}
