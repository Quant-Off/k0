//! ACPI 정적 테이블에서 커널이 필요한 디바이스를 읽는 모듈입니다.
//!
//! # Features
//! 펌웨어가 디바이스 트리를 주지 않을 때, 로더가 디바이스 트리를 만들 재료를
//! ACPI의 정적 테이블에서 얻습니다. 읽는 것은 RSDP, XSDT, MADT(인터럽트
//! 컨트롤러), GTDT(타이머 인터럽트), SPCR(콘솔)뿐입니다. AML은 해석하지
//! 않습니다. 메모리는 여기서 얻지 않습니다(UEFI 메모리 맵이 유일한 출처).
//!
//! 테이블은 펌웨어가 준 입력이라 틀릴 수 있습니다. 길이와 체크섬과 범위를
//! 확인하고, 규칙에 맞지 않으면 추측하지 않고 거부합니다. 콘솔만은 예외로,
//! 커널에 드라이버가 없는 종류면 콘솔 없이 진행합니다(커널이 이미 그렇게
//! 부팅함). 물리 메모리를 읽는 일은 호출자가 준 [Memory]를 통해서만 합니다.

use k0_boot::{MemRegion, MAX_REDIST_REGIONS};

/// 물리 주소의 바이트를 읽게 해 주는 통로입니다. 로더는 읽어도 되는 범위만
/// 내주고, 테스트는 만든 테이블 이미지를 내줍니다.
pub trait Memory {
    /// `addr`부터 `len`바이트를 주는 함수입니다. 읽으면 안 되는 범위면 `None`
    fn bytes(&self, addr: u64, len: usize) -> Option<&[u8]>;
}

/// SPCR이 가리킨 콘솔의 종류입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uart {
    Pl011,
    Sbsa,
    /// 레지스터 폭 8비트, 간격 1바이트의 16550
    Ns16550,
}

/// 콘솔 UART 하나입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Serial {
    pub kind: Uart,
    pub reg: MemRegion,
    /// 콘솔의 인터럽트 번호(GSIV). SPCR이 GIC의 SPI 하나로 밝힌 경우에만 있음
    pub irq: Option<u32>,
}

/// 인터럽트 컨트롤러입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gic {
    V2 { dist: MemRegion, cpu: MemRegion },
    V3 { dist: MemRegion, redist: [MemRegion; MAX_REDIST_REGIONS], count: usize },
}

/// 테이블에서 읽어 낸 결과입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub serial: Option<Serial>,
    pub gic: Gic,
    /// EL1 가상 타이머의 인터럽트 번호(16 이상 31 이하)
    pub timer_gsiv: u32,
    /// 타이머 인터럽트가 에지 트리거인지
    pub timer_edge: bool,
    /// 타이머 인터럽트가 액티브 로우인지
    pub timer_low: bool,
}

/// 테이블을 받아들이지 못한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpiError {
    /// RSDP나 XSDT가 읽을 수 없는 곳에 있음
    Unreadable,
    /// RSDP의 서명, 개정, 체크섬, XSDT 주소가 맞지 않음
    BadRsdp,
    /// XSDT의 서명, 길이, 체크섬이 맞지 않거나 항목이 너무 많음
    BadXsdt,
    /// 쓰려는 테이블의 길이나 체크섬이 맞지 않음
    BadTable,
    /// MADT가 없음
    NoMadt,
    /// MADT의 항목 길이가 테이블을 벗어남
    BadMadt,
    /// 디스트리뷰터 항목이 없거나 둘 이상임
    BadDistributor,
    /// 지원하지 않는 GIC 버전(2, 3, 4가 아님)
    UnsupportedGic(u8),
    /// GICv2의 CPU 인터페이스 주소가 없거나 CPU마다 다름
    BadCpuInterface,
    /// GICv3의 리디스트리뷰터를 찾지 못했거나 길이가 0이거나 4개를 넘음
    BadRedistributor,
    /// GTDT가 없음
    NoGtdt,
    /// 가상 타이머의 인터럽트 번호가 16에서 31 사이가 아님
    BadTimer,
    /// 테이블이 말한 디바이스 구간이 메모리 맵의 RAM이나 펌웨어 메모리와 겹침
    DeviceInMemory,
}

const HEADER: usize = 36;
const MAX_TABLE: usize = 1024 * 1024;
const MAX_XSDT_ENTRIES: usize = 1024;
/// MPIDR에서 어피니티 필드만 남기는 마스크(Aff3과 Aff2..Aff0)
const MPIDR_AFFINITY: u64 = 0xFF_00FF_FFFF;

fn le32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le64(b: &[u8], off: usize) -> Option<u64> {
    let s = b.get(off..off.checked_add(8)?)?;
    Some(u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn sum_is_zero(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

/// 서명이 `want`인 테이블을 통째로 읽는 함수입니다. 길이와 체크섬을 확인합니다.
fn table<'a>(mem: &'a impl Memory, addr: u64, want: &[u8; 4]) -> Result<Option<&'a [u8]>, AcpiError> {
    // 읽을 수 없는 항목은 우리가 쓰는 테이블인지 알 수 없으므로 건너뜀
    let Some(head) = mem.bytes(addr, HEADER) else { return Ok(None) };
    if &head[..4] != want {
        return Ok(None);
    }
    let len = le32(head, 4).ok_or(AcpiError::BadTable)? as usize;
    if !(HEADER..=MAX_TABLE).contains(&len) {
        return Err(AcpiError::BadTable);
    }
    let all = mem.bytes(addr, len).ok_or(AcpiError::BadTable)?;
    if !sum_is_zero(all) {
        return Err(AcpiError::BadTable);
    }
    Ok(Some(all))
}

/// MADT에서 읽어 모은 값입니다.
struct Madt {
    dist: Option<(u64, u8)>,
    dist_count: usize,
    gicr: [MemRegion; MAX_REDIST_REGIONS],
    gicr_count: usize,
    gicr_overflow: bool,
    /// 켜진 CPU 인터페이스 항목들의 주소가 모두 같으면 그 값
    cpu_base: Option<u64>,
    cpu_mismatch: bool,
    /// CPU 항목들이 말한 리디스트리뷰터 프레임을 맞닿은 것끼리 합친 구간
    cpu_gicr: [MemRegion; MAX_REDIST_REGIONS],
    cpu_gicr_count: usize,
    cpu_gicr_bad: bool,
    /// 부트 CPU의 항목이 리디스트리뷰터 주소를 가지고 있는지
    boot_has_gicr: bool,
}

fn read_madt(t: &[u8], boot_mpidr: u64) -> Result<Madt, AcpiError> {
    let mut m = Madt {
        dist: None,
        dist_count: 0,
        gicr: [MemRegion { base: 0, size: 0 }; MAX_REDIST_REGIONS],
        gicr_count: 0,
        gicr_overflow: false,
        cpu_base: None,
        cpu_mismatch: false,
        cpu_gicr: [MemRegion { base: 0, size: 0 }; MAX_REDIST_REGIONS],
        cpu_gicr_count: 0,
        cpu_gicr_bad: false,
        boot_has_gicr: false,
    };
    let mut off = 44;
    while off < t.len() {
        let kind = *t.get(off).ok_or(AcpiError::BadMadt)?;
        let len = *t.get(off + 1).ok_or(AcpiError::BadMadt)? as usize;
        let entry = t.get(off..off + len).filter(|_| len >= 2).ok_or(AcpiError::BadMadt)?;
        match kind {
            // GICC: CPU 인터페이스. 켜진(flags 비트 0) 항목만 봄. ACPI 5.0의 짧은
            // 항목(40바이트)에는 리디스트리뷰터와 MPIDR 필드가 없음
            0xB if len >= 40 => {
                let field = |o| le64(entry, o).ok_or(AcpiError::BadMadt);
                let flags = le32(entry, 12).ok_or(AcpiError::BadMadt)?;
                // 비트 0은 켜짐, 비트 3은 나중에 켤 수 있음(ACPI 6.3). 리디스트리뷰터
                // 프레임은 둘 다에서 모으고, CPU 인터페이스 주소는 켜진 것만 봄
                if flags & 0x9 != 0 {
                    if flags & 1 != 0 {
                        let base = field(32)?;
                        match m.cpu_base {
                            None if !m.cpu_mismatch => m.cpu_base = Some(base),
                            Some(b) if b == base => {}
                            _ => {
                                m.cpu_mismatch = true;
                                m.cpu_base = None;
                            }
                        }
                    }
                    let gicr = if len >= 76 { field(60)? } else { 0 };
                    if gicr != 0 {
                        // CPU마다 프레임 하나(RD와 SGI, 128KiB). 앞 구간에 맞닿으면 잇고,
                        // 아니면 새 구간. 같은 주소가 두 번 나오거나 4개를 넘으면 거부
                        let frame = MemRegion { base: gicr, size: 0x2_0000 };
                        let n = m.cpu_gicr_count;
                        let seen = m.cpu_gicr[..n].iter().any(|r| r.base <= gicr && gicr - r.base < r.size);
                        if seen || gicr.checked_add(frame.size).is_none() {
                            m.cpu_gicr_bad = true;
                        } else if n > 0 && gicr.checked_sub(m.cpu_gicr[n - 1].base + m.cpu_gicr[n - 1].size).is_some_and(|gap| gap == 0 || gap == 0x2_0000) {
                            // 맞닿았거나(v3) 가상 LPI 프레임 두 장만큼 떨어진(v4) 다음 프레임
                            m.cpu_gicr[n - 1].size = gicr + frame.size - m.cpu_gicr[n - 1].base;
                        } else if n < MAX_REDIST_REGIONS {
                            m.cpu_gicr[n] = frame;
                            m.cpu_gicr_count += 1;
                        } else {
                            m.cpu_gicr_bad = true;
                        }
                        if len >= 76 && field(68)? & MPIDR_AFFINITY == boot_mpidr & MPIDR_AFFINITY {
                            m.boot_has_gicr = true;
                        }
                    }
                }
            }
            0xB => return Err(AcpiError::BadMadt),
            // GICD: 디스트리뷰터
            0xC if len >= 24 => {
                m.dist = Some((le64(entry, 8).ok_or(AcpiError::BadMadt)?, entry[20]));
                m.dist_count += 1;
            }
            0xC => return Err(AcpiError::BadMadt),
            // GICR: 리디스트리뷰터 구간
            0xE if len >= 16 => {
                let base = le64(entry, 4).ok_or(AcpiError::BadMadt)?;
                let size = u64::from(le32(entry, 12).ok_or(AcpiError::BadMadt)?);
                if m.gicr_count < MAX_REDIST_REGIONS {
                    m.gicr[m.gicr_count] = MemRegion { base, size };
                    m.gicr_count += 1;
                } else {
                    m.gicr_overflow = true;
                }
            }
            0xE => return Err(AcpiError::BadMadt),
            _ => {}
        }
        off += len;
    }
    Ok(m)
}

fn gic_of(m: &Madt) -> Result<Gic, AcpiError> {
    let (dist_base, version) = match (m.dist, m.dist_count) {
        (Some(d), 1) => d,
        _ => return Err(AcpiError::BadDistributor),
    };
    let described = m.gicr_count > 0 || m.cpu_gicr_count > 0;
    let v3 = match version {
        2 => false,
        3 | 4 => true,
        // 0은 "버전을 적지 않음". 리디스트리뷰터가 기술돼 있으면 v3로 봄
        0 => described,
        other => return Err(AcpiError::UnsupportedGic(other)),
    };
    // 정렬과 최소 크기는 커널의 발견 코드가 요구하는 것과 같게 여기서 거름.
    // 프레임은 v2가 4KiB, v3가 64KiB 단위이고 주소 계산이 넘치면 안 됨
    let unit: u64 = if v3 { 0x1_0000 } else { 0x1000 };
    let placed = |base: u64, size: u64| base != 0 && base % unit == 0 && base.checked_add(size).is_some();
    if !placed(dist_base, unit) {
        return Err(AcpiError::BadDistributor);
    }
    if !v3 {
        let cpu = m.cpu_base.filter(|&b| placed(b, 0x2000)).ok_or(AcpiError::BadCpuInterface)?;
        // 크기는 테이블에 없음. Linux가 ACPI에서 쓰는 값(4KiB, 8KiB)을 따름
        return Ok(Gic::V2 {
            dist: MemRegion { base: dist_base, size: 0x1000 },
            cpu: MemRegion { base: cpu, size: 0x2000 },
        });
    }
    // 구간 항목(GICR)이 하나라도 있으면 그것만 쓰고 CPU 항목의 주소는 보지 않음
    let (redist, count) = if m.gicr_count > 0 {
        if m.gicr_overflow || m.gicr[..m.gicr_count].iter().any(|r| r.base == 0 || r.size == 0) {
            return Err(AcpiError::BadRedistributor);
        }
        (m.gicr, m.gicr_count)
    } else {
        // 부트 CPU의 프레임이 그 안에 있어야 커널이 자기 리디스트리뷰터를 찾음
        if m.cpu_gicr_count == 0 || m.cpu_gicr_bad || !m.boot_has_gicr {
            return Err(AcpiError::BadRedistributor);
        }
        (m.cpu_gicr, m.cpu_gicr_count)
    };
    if redist[..count].iter().any(|r| r.size < 0x2_0000 || !placed(r.base, r.size)) {
        return Err(AcpiError::BadRedistributor);
    }
    let dist = MemRegion { base: dist_base, size: 0x1_0000 };
    let all = || core::iter::once(&dist).chain(&redist[..count]);
    for (i, a) in all().enumerate() {
        if all().skip(i + 1).any(|b| a.base < b.base + b.size && b.base < a.base + a.size) {
            return Err(AcpiError::BadRedistributor);
        }
    }
    Ok(Gic::V3 { dist, redist, count })
}

fn serial_of(t: &[u8]) -> Option<Serial> {
    if t.len() < 52 {
        return None;
    }
    let (interface, space, width, access) = (t[36], t[40], t[41], t[43]);
    let base = le64(t, 44)?;
    // 시스템 메모리 공간의 0이 아닌 주소만 받음
    if space != 0 || base == 0 {
        return None;
    }
    let (kind, size) = match interface {
        0x03 => (Uart::Pl011, 0x1000),
        0x0D | 0x0E => (Uart::Sbsa, 0x1000),
        // 16550 계열은 레지스터 간격을 SPCR로 알 수 없음. 폭 8비트에 바이트
        // 접근(또는 지정 없음)일 때만 간격 1바이트로 받고, 나머지는 추측하지 않음
        0x00 | 0x01 | 0x12 if width == 8 && access <= 1 => (Uart::Ns16550, 8),
        _ => return None,
    };
    base.checked_add(size)?;
    // PL011과 SBSA UART의 레지스터는 32비트 단위로 접근함
    if kind != Uart::Ns16550 && base % 4 != 0 {
        return None;
    }
    // 인터럽트: 종류가 GIC 하나뿐이고(비트 3만) 번호가 SPI일 때만 받음. 종류는
    // 오프셋 52, 번호는 54부터 4바이트라서 테이블이 58바이트보다 짧으면
    // 인터럽트 없이 콘솔만 받음
    let irq = match (t.get(52), le32(t, 54)) {
        (Some(&0x08), Some(gsiv)) if (32..=1019).contains(&gsiv) => Some(gsiv),
        _ => None,
    };
    Some(Serial {
        kind,
        reg: MemRegion { base, size },
        irq,
    })
}

/// ACPI 정적 테이블에서 콘솔, 인터럽트 컨트롤러, 타이머 인터럽트를 읽는
/// 함수입니다.
///
/// # Arguments
/// `mem` - 물리 메모리를 읽는 통로
/// `rsdp` - 펌웨어의 구성 테이블이 준 RSDP의 물리 주소
/// `boot_mpidr` - 이 CPU의 MPIDR_EL1(리디스트리뷰터를 CPU 항목에서 찾을 때 씀)
///
/// # Errors
/// 테이블이 읽을 수 없는 곳에 있거나 형식과 규칙에 맞지 않으면 `AcpiError`
pub fn read(mem: &impl Memory, rsdp: u64, boot_mpidr: u64) -> Result<Found, AcpiError> {
    let head = mem.bytes(rsdp, HEADER).ok_or(AcpiError::Unreadable)?;
    if &head[..8] != b"RSD PTR " || head[15] < 2 || !sum_is_zero(&head[..20]) {
        return Err(AcpiError::BadRsdp);
    }
    let len = le32(head, 20).ok_or(AcpiError::BadRsdp)? as usize;
    if !(HEADER..=4096).contains(&len) {
        return Err(AcpiError::BadRsdp);
    }
    if !sum_is_zero(mem.bytes(rsdp, len).ok_or(AcpiError::Unreadable)?) {
        return Err(AcpiError::BadRsdp);
    }
    let xsdt_at = le64(head, 24).filter(|&a| a != 0).ok_or(AcpiError::BadRsdp)?;

    let xhead = mem.bytes(xsdt_at, HEADER).ok_or(AcpiError::Unreadable)?;
    let xlen = le32(xhead, 4).ok_or(AcpiError::BadXsdt)? as usize;
    if &xhead[..4] != b"XSDT" || xlen < HEADER || (xlen - HEADER) % 8 != 0 || (xlen - HEADER) / 8 > MAX_XSDT_ENTRIES {
        return Err(AcpiError::BadXsdt);
    }
    let xsdt = mem.bytes(xsdt_at, xlen).ok_or(AcpiError::Unreadable)?;
    if !sum_is_zero(xsdt) {
        return Err(AcpiError::BadXsdt);
    }

    // 서명마다 처음 나온 테이블을 씀
    let (mut madt, mut gtdt, mut spcr) = (None, None, None);
    for entry in xsdt[HEADER..].chunks_exact(8) {
        let addr = le64(entry, 0).ok_or(AcpiError::BadXsdt)?;
        if madt.is_none() {
            madt = table(mem, addr, b"APIC")?;
        }
        if gtdt.is_none() {
            gtdt = table(mem, addr, b"GTDT")?;
        }
        // 콘솔은 없어도 되므로, 길이나 체크섬이 틀린 SPCR은 없는 것으로 봄
        if spcr.is_none() {
            spcr = table(mem, addr, b"SPCR").ok().flatten();
        }
    }

    let gic = gic_of(&read_madt(madt.ok_or(AcpiError::NoMadt)?, boot_mpidr)?)?;
    let gtdt = gtdt.ok_or(AcpiError::NoGtdt)?;
    let timer_gsiv = le32(gtdt, 64).ok_or(AcpiError::BadTimer)?;
    let flags = le32(gtdt, 68).ok_or(AcpiError::BadTimer)?;
    if !(16..=31).contains(&timer_gsiv) {
        return Err(AcpiError::BadTimer);
    }
    Ok(Found {
        serial: spcr.and_then(serial_of),
        gic,
        timer_gsiv,
        timer_edge: flags & 1 != 0,
        timer_low: flags & 2 != 0,
    })
}

impl Found {
    /// 테이블이 말한 디바이스 구간들을 차례로 주는 함수입니다.
    pub fn devices(&self) -> impl Iterator<Item = MemRegion> + '_ {
        let (dist, cpu) = match &self.gic {
            Gic::V2 { dist, cpu } => (*dist, Some(*cpu)),
            Gic::V3 { dist, .. } => (*dist, None),
        };
        let redist: &[MemRegion] = match &self.gic {
            Gic::V3 { redist, count, .. } => &redist[..*count],
            Gic::V2 { .. } => &[],
        };
        self.serial.iter().map(|s| s.reg).chain([dist]).chain(cpu).chain(redist.iter().copied())
    }

    /// 디바이스 구간이 메모리 맵에 비추어 디바이스일 수 있는지 확인하는
    /// 함수입니다.
    ///
    /// 펌웨어의 디바이스 트리가 말한 디바이스는 RAM에서 빼고 받아들이지만,
    /// 테이블에서 합성한 값은 더 엄격하게 봅니다. 정렬 단위로 넓힌 구간이 맵의
    /// 어느 디스크립터와 겹친다면 그 디스크립터는 디바이스 레지스터(MMIO)여야
    /// 합니다. 맵에 없는 주소는 괜찮습니다(QEMU가 그러함).
    ///
    /// # Arguments
    /// `map` - UEFI 메모리 맵
    ///
    /// # Errors
    /// RAM이나 펌웨어 메모리와 겹치면 `DeviceInMemory`
    pub fn check_devices(&self, map: &crate::memmap::Map<'_>) -> Result<(), AcpiError> {
        self.devices().try_for_each(|d| Self::outside_memory(&d, map))
    }

    /// 콘솔이 메모리 위에 있다고 적혀 있으면 콘솔만 버린 결과를 주는 함수입니다.
    /// 콘솔은 없어도 부팅할 수 있으므로 거부 사유로 삼지 않습니다.
    ///
    /// # Arguments
    /// `map` - UEFI 메모리 맵
    pub fn without_console_in_memory(mut self, map: &crate::memmap::Map<'_>) -> Self {
        if self.serial.is_some_and(|s| Self::outside_memory(&s.reg, map).is_err()) {
            self.serial = None;
        }
        self
    }

    fn outside_memory(d: &MemRegion, map: &crate::memmap::Map<'_>) -> Result<(), AcpiError> {
        let align = crate::memmap::ALIGN;
        let start = d.base & !(align - 1);
        let end = d.base.checked_add(d.size).and_then(|e| e.checked_add(align - 1)).ok_or(AcpiError::DeviceInMemory)?
            & !(align - 1);
        if map.iter().any(|m| !m.is_mmio() && m.start < end && start < m.end) {
            return Err(AcpiError::DeviceInMemory);
        }
        Ok(())
    }
}

