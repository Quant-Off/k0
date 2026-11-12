//! ACPI에서 읽은 값으로 최소한의 디바이스 트리를 만드는 모듈입니다.
//!
//! # Features
//! 커널은 플랫폼을 디바이스 트리에서만 압니다. 펌웨어가 트리를 주지 않으면
//! [crate::acpi]가 읽은 콘솔, 인터럽트 컨트롤러, 타이머 인터럽트로 커널의
//! 발견 코드가 받아들이는 모양의 트리를 여기서 만듭니다. 만든 트리는 펌웨어가
//! 준 트리와 같은 경로([crate::prepare])를 타므로, 메모리 기술의 재작성과
//! 커널 파서로 하는 자가 검사를 그대로 거칩니다.

use k0_boot::MemRegion;

use crate::acpi::{Found, Gic, Uart};
use crate::fdtw::{WriteError, Writer};

/// 인터럽트 컨트롤러 노드의 phandle 값
const GIC_PHANDLE: u32 = 1;

/// 만든 트리의 문자열 블록에 필요한 자리(프로퍼티 이름들의 합보다 넉넉함)
const STRINGS: usize = 256;

fn cells2(out: &mut [u8], at: &mut usize, v: u64) {
    out[*at..*at + 8].copy_from_slice(&v.to_be_bytes());
    *at += 8;
}

fn reg(regions: &[MemRegion], out: &mut [u8]) -> usize {
    let mut at = 0;
    for r in regions {
        cells2(out, &mut at, r.base);
        cells2(out, &mut at, r.size);
    }
    at
}

/// `<이름>@<16진수 주소>` 꼴의 노드 이름을 버퍼에 만드는 함수입니다.
fn unit_name<'a>(prefix: &[u8], addr: u64, buf: &'a mut [u8; 32]) -> &'a [u8] {
    let n = prefix.len();
    buf[..n].copy_from_slice(prefix);
    buf[n] = b'@';
    let digits = (16 - addr.leading_zeros() as usize / 4).max(1);
    for i in 0..digits {
        let nibble = (addr >> ((digits - 1 - i) * 4)) & 0xF;
        buf[n + 1 + i] = b"0123456789abcdef"[nibble as usize];
    }
    &buf[..n + 1 + digits]
}

/// ACPI에서 읽은 값으로 디바이스 트리를 만드는 함수입니다.
///
/// 메모리 노드에는 자리표시 구간 하나만 넣습니다. 커널의 파서가 메모리 없는
/// 트리를 거부하기 때문이고, 재작성 단계가 이 노드를 버리고 UEFI 메모리 맵에서
/// 계획한 구간으로 바꿉니다.
///
/// # Arguments
/// `found` - ACPI 테이블에서 읽은 값
/// `placeholder` - 메모리 노드에 넣을 자리표시 구간(로더가 가진 메모리)
/// `out` - 출력 버퍼
///
/// # Errors
/// 버퍼가 모자라면 `WriteError`
pub fn synthesize(found: &Found, placeholder: MemRegion, out: &mut [u8]) -> Result<usize, WriteError> {
    let mut name = [0u8; 32];
    let mut value = [0u8; 16 * 5];
    let mut w = Writer::new(out, &[], STRINGS)?;
    w.begin(b"")?;
    w.prop(b"#address-cells", &2u32.to_be_bytes())?;
    w.prop(b"#size-cells", &2u32.to_be_bytes())?;
    w.prop(b"interrupt-parent", &GIC_PHANDLE.to_be_bytes())?;

    if let Some(serial) = &found.serial {
        // stdout-path는 아래 콘솔 노드의 절대 경로
        let mut path = [0u8; 34];
        path[0] = b'/';
        let node = unit_name(b"serial", serial.reg.base, &mut name);
        path[1..1 + node.len()].copy_from_slice(node);
        w.begin(b"chosen")?;
        w.prop(b"stdout-path", &path[..node.len() + 2])?;
        w.end()?;
    }

    w.begin(unit_name(b"memory", placeholder.base, &mut name))?;
    w.prop(b"device_type", b"memory\0")?;
    let n = reg(&[placeholder], &mut value);
    w.prop(b"reg", &value[..n])?;
    w.end()?;

    if let Some(serial) = &found.serial {
        w.begin(unit_name(b"serial", serial.reg.base, &mut name))?;
        let compatible: &[u8] = match serial.kind {
            Uart::Pl011 => b"arm,pl011\0arm,primecell\0",
            Uart::Sbsa => b"arm,sbsa-uart\0",
            Uart::Ns16550 => b"ns16550a\0",
        };
        w.prop(b"compatible", compatible)?;
        let n = reg(&[serial.reg], &mut value);
        w.prop(b"reg", &value[..n])?;
        if serial.kind == Uart::Ns16550 {
            w.prop(b"reg-shift", &0u32.to_be_bytes())?;
            w.prop(b"reg-io-width", &1u32.to_be_bytes())?;
        }
        w.end()?;
    }

    let (dist, compatible): (MemRegion, &[u8]) = match &found.gic {
        Gic::V2 { dist, .. } => (*dist, b"arm,gic-400\0"),
        Gic::V3 { dist, .. } => (*dist, b"arm,gic-v3\0"),
    };
    w.begin(unit_name(b"intc", dist.base, &mut name))?;
    w.prop(b"compatible", compatible)?;
    w.prop(b"#interrupt-cells", &3u32.to_be_bytes())?;
    w.prop(b"interrupt-controller", &[])?;
    w.prop(b"phandle", &GIC_PHANDLE.to_be_bytes())?;
    let n = match &found.gic {
        Gic::V2 { dist, cpu } => reg(&[*dist, *cpu], &mut value),
        Gic::V3 { dist, redist, count } => {
            if *count > 1 {
                w.prop(b"#redistributor-regions", &(*count as u32).to_be_bytes())?;
            }
            let mut all = [*dist; 5];
            all[1..1 + count].copy_from_slice(&redist[..*count]);
            reg(&all[..1 + count], &mut value)
        }
    };
    w.prop(b"reg", &value[..n])?;
    w.end()?;

    // 인터럽트 지정자: 종류 1(PPI), PPI 번호, 트리거 플래그. 이름으로 "virt"임을 밝힘
    let flags: u32 = match (found.timer_edge, found.timer_low) {
        (false, false) => 4,
        (false, true) => 8,
        (true, false) => 1,
        (true, true) => 2,
    };
    let mut spec = [0u8; 12];
    spec[..4].copy_from_slice(&1u32.to_be_bytes());
    spec[4..8].copy_from_slice(&(found.timer_gsiv - 16).to_be_bytes());
    spec[8..].copy_from_slice(&flags.to_be_bytes());
    w.begin(b"timer")?;
    w.prop(b"compatible", b"arm,armv8-timer\0")?;
    w.prop(b"interrupts", &spec)?;
    w.prop(b"interrupt-names", b"virt\0")?;
    w.end()?;

    w.end()?;
    w.finish(0)
}
