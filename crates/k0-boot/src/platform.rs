//! DTB에서 커널이 직접 다루는 디바이스를 찾아 플랫폼 기술서를 만드는 모듈입니다.
//!
//! # Features
//! 커널이 다루는 디바이스는 인터럽트 컨트롤러, 타이머, 콘솔 셋뿐입니다. 이
//! 모듈은 DTB가 이름으로 가리킨 것만 찾습니다. 타이머 노드에서 출발해 그
//! 인터럽트 부모를 인터럽트 컨트롤러로 삼고, `/chosen`의 stdout-path가 가리킨
//! 노드만 콘솔로 삼습니다. "첫 번째 UART" 같은 추측은 하지 않습니다. 결과는
//! 불변 값([Platform]) 하나이며, 커널은 이 값만 보고 드라이버를 고릅니다.
//!
//! 콘솔을 못 찾는 것은 오류가 아니라 콘솔 없는 부팅이고([NoConsole]로 이유를
//! 남김), 인터럽트 컨트롤러와 타이머는 못 찾으면 부팅을 거부할 오류입니다.
//!
//! # Errors
//! DTB 구조 위반은 `DiscoverError::Dtb`, 규약에 맞지 않는 하드웨어 기술은
//! 나머지 `DiscoverError`로 반환합니다. 호출자는 실패 시 부팅을 중단해야
//! 합니다(fail-secure).

use core::fmt;
use core::ops::Range;

use crate::fdt::walk::{alias, read_cells, strings, walk, Fdt, Flow, Levels, Node, MAX_DEPTH, P};
use crate::fdt::{dtb_span, BootError, MemRegion};

/// 받아들이는 GICv3 리디스트리뷰터 구간 수의 상한
pub const MAX_REDIST_REGIONS: usize = 4;

/// PL011 드라이버가 건드리는 레지스터 범위(FR이 0x18에 있는 32비트 레지스터)
const PL011_SPAN: u64 = 0x1C;

/// 16550의 LSR 레지스터 번호(레지스터 간격은 reg-shift가 정함)
const NS16550_LSR: u64 = 5;

/// 물리 주소 구간 하나를 나타내는 구조체입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub base: u64,
    pub size: u64,
}

impl Region {
    const EMPTY: Region = Region { base: 0, size: 0 };

    fn end(&self) -> u64 {
        self.base + self.size
    }
}

/// 콘솔 UART의 종류를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleKind {
    Pl011,
    /// PL011의 레지스터 배치를 따르지만 식별 레지스터가 보장되지 않는 UART
    SbsaUart,
    /// 레지스터 n의 위치는 `n << reg_shift`, 접근 폭은 `io_width`바이트
    Ns16550 { reg_shift: u8, io_width: u8 },
}

/// 콘솔 UART 하나를 나타내는 구조체입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Console {
    pub kind: ConsoleKind,
    /// 레지스터 0의 물리 주소 (reg-offset 반영)
    pub base: u64,
    /// 드라이버가 `base`부터 건드리는 바이트 수
    pub span: u64,
    /// DTB가 선언한 레지스터 구간 전체 (겹침 검사용)
    pub reg: Region,
}

/// 인터럽트 컨트롤러를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intc {
    GicV2 {
        dist: Region,
        cpu: Region,
    },
    GicV3 {
        dist: Region,
        redist: [Region; MAX_REDIST_REGIONS],
        redist_count: usize,
        /// 리디스트리뷰터 프레임 간격, 0이면 DTB에 없음(하드웨어에서 정함)
        stride: u64,
    },
}

impl Intc {
    /// GICv3의 리디스트리뷰터 구간들을 주는 함수입니다. GICv2면 빈 목록입니다.
    pub fn redist(&self) -> &[Region] {
        match self {
            Intc::GicV3 { redist, redist_count, .. } => &redist[..*redist_count],
            Intc::GicV2 { .. } => &[],
        }
    }
}

/// 타이머 인터럽트를 나타내는 구조체입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timer {
    /// EL1 가상 타이머의 INTID
    pub intid: u32,
    /// DTB의 clock-frequency, 0이면 없음 (CNTFRQ_EL0가 0일 때만 쓰임)
    pub clock_hz: u32,
}

/// 발견한 플랫폼 전체를 나타내는 구조체입니다. 만든 뒤에는 바뀌지 않습니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platform {
    pub console: Option<Console>,
    pub intc: Intc,
    pub timer: Timer,
}

/// 콘솔이 없는 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoConsole {
    NoStdoutPath,
    BadPath,
    UnknownAlias,
    NotFound,
    Ambiguous,
    Disabled,
    UnsupportedUart,
    BadProperties,
    NotTranslatable,
}

/// 플랫폼 발견이 거부된 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoverError {
    Dtb(BootError),
    NoTimer,
    MultipleTimers,
    TimerErratum,
    TimerInterruptsExtended,
    BadTimerInterrupts,
    BadTimerClock,
    NoInterruptParent,
    UnsupportedInterruptTopology,
    IntcNotFound,
    DuplicatePhandle,
    IntcDisabled,
    IntcNotController,
    IntcInterruptMap,
    UnsupportedIntc,
    BadIntcCells,
    BadIntcReg,
    IntcNotTranslatable,
    NonBankedGic,
    BadRedistributor,
}

impl From<BootError> for DiscoverError {
    fn from(e: BootError) -> Self {
        DiscoverError::Dtb(e)
    }
}

/// 발견 결과와 콘솔이 없을 때의 이유를 함께 담는 구조체입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Discovery {
    pub platform: Platform,
    pub no_console: Option<NoConsole>,
}

/// 물리 주소의 DTB에서 플랫폼을 발견하는 함수입니다.
///
/// [crate::parse]와 같은 방식으로 읽습니다. 재배치된 데이터를 쓰므로
/// higher-half 점프 이후에만 호출할 수 있습니다.
///
/// # Arguments
/// `dtb_phys` - 부트로더가 x0로 넘긴 DTB 물리 주소
/// `read_offset` - 읽기 주소에 더할 오프셋(TTBR1 별칭이면 `KERNEL_VA_OFFSET`)
///
/// # Errors
/// 구조 위반과 규약에 맞지 않는 하드웨어 기술은 `DiscoverError`
pub fn discover(dtb_phys: usize, read_offset: usize) -> Result<Discovery, DiscoverError> {
    let read_base = dtb_phys.checked_add(read_offset).ok_or(BootError::BadHeader)?;
    let span = dtb_span(read_base)?;
    // SAFETY: [span.start, span.end)는 dtb_span의 상한 검증을 통과했고 호출자가
    //         이 범위를 읽기 전용으로 매핑해 둔 상태임 (parse와 같은 전제)
    let blob = unsafe { core::slice::from_raw_parts(span.start as *const u8, span.end - span.start) };
    discover_blob(blob)
}

/// 타이머 노드에서 읽어 둔 값들입니다.
#[derive(Clone, Copy)]
struct TimerNode<'a> {
    interrupts: &'a [u8],
    names: Option<&'a [u8]>,
    clock: Option<u32>,
    parent: u32,
}

/// 메모리에 있는 DTB 바이트열에서 플랫폼을 발견하는 함수입니다.
///
/// [discover]가 주소로 읽어 온 뒤 하는 일과 같습니다. 커널 밖의 코드가 커널과
/// 같은 규칙으로 판정할 때 씁니다.
///
/// # Arguments
/// `blob` - DTB 전체(헤더의 totalsize 길이)
///
/// # Errors
/// 구조 위반과 규약에 맞지 않는 하드웨어 기술은 `DiscoverError`
pub fn discover_blob(blob: &[u8]) -> Result<Discovery, DiscoverError> {
    let fdt = Fdt::new(blob)?;

    // 1차 순회: /chosen의 stdout-path와 타이머 노드
    let mut stdout: Option<&[u8]> = None;
    let mut timer: Option<TimerNode<'_>> = None;
    walk(&fdt, |levels, node, depth| -> Result<Flow, DiscoverError> {
        if depth == 2 && node.name == b"chosen" {
            stdout = node.get(P::StdoutPath).or(node.get(P::LinuxStdoutPath));
        }
        let is_timer = node.compatible_has(b"arm,armv8-timer") || node.compatible_has(b"arm,armv7-timer");
        if is_timer && usable(levels, node, depth) {
            if timer.is_some() {
                return Err(DiscoverError::MultipleTimers);
            }
            timer = Some(read_timer(levels, node, depth)?);
        }
        Ok(Flow::Continue)
    })?;
    let timer = timer.ok_or(DiscoverError::NoTimer)?;

    // 2차 순회: 타이머의 인터럽트 부모(phandle은 트리 전체에서 유일해야 함)
    // 일치한 노드는 그 자리에서 해석해 결과만 남김(조상 문맥을 복사해 두지 않음)
    let mut intc: Option<Result<(Intc, u32), DiscoverError>> = None;
    let mut matches = 0usize;
    walk(&fdt, |levels, node, depth| -> Result<Flow, DiscoverError> {
        if node.phandle()? == Some(timer.parent) {
            matches += 1;
            if intc.is_none() {
                intc = Some(read_intc(levels, node, depth));
            }
        }
        Ok(Flow::Continue)
    })?;
    // phandle이 유일하지 않으면 어느 노드인지 고르지 않음
    if matches > 1 {
        return Err(DiscoverError::DuplicatePhandle);
    }
    let (intc, interrupt_cells) = intc.ok_or(DiscoverError::IntcNotFound)??;
    let timer = Timer {
        intid: timer_intid(&timer, interrupt_cells)?,
        clock_hz: timer.clock.unwrap_or(0),
    };

    let (console, no_console) = match find_console(&fdt, stdout)? {
        Ok(c) => (Some(c), None),
        Err(why) => (None, Some(why)),
    };

    Ok(Discovery {
        platform: Platform { console, intc, timer },
        no_console,
    })
}

/// 노드와 그 조상이 모두 사용 가능한 상태인지 확인하는 함수입니다.
fn usable(levels: &Levels<'_>, node: &Node<'_>, depth: usize) -> bool {
    node.available() && levels[1..depth].iter().all(|l| l.available)
}

fn read_timer<'a>(
    levels: &Levels<'a>,
    node: &Node<'a>,
    depth: usize,
) -> Result<TimerNode<'a>, DiscoverError> {
    // 타이머 정오표는 카운터 읽기와 TVAL 쓰기를 못 믿게 만드는데 우회 코드가
    // 없으므로, 선언된 보드에서는 부팅하지 않음
    if node.has(P::ErratumA008585) || node.has(P::Erratum161010101) || node.has(P::ErratumUnknown1) {
        return Err(DiscoverError::TimerErratum);
    }
    if node.has(P::InterruptsExtended) {
        return Err(DiscoverError::TimerInterruptsExtended);
    }
    let interrupts = node.get(P::Interrupts).ok_or(DiscoverError::BadTimerInterrupts)?;
    let clock = match node.get(P::ClockFrequency) {
        None => None,
        Some(v) if v.len() == 4 => Some(read_cells(v) as u32),
        Some(_) => return Err(DiscoverError::BadTimerClock),
    };

    // 인터럽트 부모: 노드 자신의 interrupt-parent, 없으면 트리 부모로 올라감.
    // 올라가다 만난 조상이 스스로 인터럽트 컨트롤러면(#interrupt-cells) 그것이
    // 부모인데, phandle 없이는 2차 순회에서 가리킬 수 없어 지원하지 않음
    let mut parent = node.u32(P::InterruptParent)?;
    let mut k = depth;
    while parent.is_none() && k > 1 {
        k -= 1;
        if levels[k].has_interrupt_cells {
            return Err(DiscoverError::UnsupportedInterruptTopology);
        }
        parent = levels[k].interrupt_parent()?;
    }
    Ok(TimerNode {
        interrupts,
        names: node.get(P::InterruptNames),
        clock,
        parent: parent.ok_or(DiscoverError::NoInterruptParent)?,
    })
}

/// 타이머 노드의 interrupts에서 EL1 가상 타이머의 INTID를 얻는 함수입니다.
///
/// interrupt-names가 있으면 "virt" 항목을, 없으면 바인딩이 정한 순서의
/// 세 번째(인덱스 2) 항목을 씁니다. 번호는 아키텍처가 고정하지 않으므로
/// 상수로 가정하지 않습니다.
fn timer_intid(timer: &TimerNode<'_>, cells: u32) -> Result<u32, DiscoverError> {
    let entry = cells as usize * 4;
    if timer.interrupts.is_empty() || timer.interrupts.len() % entry != 0 {
        return Err(DiscoverError::BadTimerInterrupts);
    }
    let count = timer.interrupts.len() / entry;
    if count > 5 {
        return Err(DiscoverError::BadTimerInterrupts);
    }
    let index = match timer.names {
        Some(names) => strings(names)
            .position(|n| n == b"virt")
            .ok_or(DiscoverError::BadTimerInterrupts)?,
        None => 2,
    };
    if index >= count {
        return Err(DiscoverError::BadTimerInterrupts);
    }
    let spec = &timer.interrupts[index * entry..(index + 1) * entry];
    let cell = |i: usize| read_cells(&spec[i * 4..i * 4 + 4]);
    // 셀 0은 종류(1 = PPI), 셀 1은 PPI 번호, 셀 2는 플래그(트리거는 하드웨어에서
    // 읽으므로 쓰지 않음), 네 번째 셀은 PPI 파티션 phandle이라 0이어야 함
    if cell(0) != 1 || cell(1) > 15 || (cells == 4 && cell(3) != 0) {
        return Err(DiscoverError::BadTimerInterrupts);
    }
    Ok(16 + cell(1) as u32)
}

/// 노드의 reg에서 `index`번째 항목을 CPU 물리 주소 구간으로 읽는 함수입니다.
fn reg_entry(
    levels: &Levels<'_>,
    node: &Node<'_>,
    depth: usize,
    index: usize,
) -> Result<Region, RegError> {
    let parent = &levels[depth - 1];
    // 루트가 셀 크기를 생략하면 DT spec 기본값(2 / 1), 그 밖의 부모는 필수
    let (ac, sc) = if depth == 2 {
        (
            parent.addr_cells().map_err(|_| RegError::Bad)?.unwrap_or(2),
            parent.size_cells().map_err(|_| RegError::Bad)?.unwrap_or(1),
        )
    } else {
        (
            parent.addr_cells().map_err(|_| RegError::Bad)?.ok_or(RegError::Bad)?,
            parent.size_cells().map_err(|_| RegError::Bad)?.ok_or(RegError::Bad)?,
        )
    };
    if !(1..=2).contains(&ac) || !(1..=2).contains(&sc) {
        return Err(RegError::Bad);
    }
    let reg = node.get(P::Reg).ok_or(RegError::Bad)?;
    let entry = ((ac + sc) * 4) as usize;
    if reg.len() % entry != 0 {
        return Err(RegError::Bad);
    }
    let chunk = reg.get(index * entry..(index + 1) * entry).ok_or(RegError::Bad)?;
    let (a, s) = chunk.split_at((ac * 4) as usize);
    let (addr, size) = (read_cells(a), read_cells(s));
    if size == 0 {
        return Err(RegError::Bad);
    }
    let base = translate(levels, depth, addr, size)?;
    base.checked_add(size).ok_or(RegError::Bad)?;
    Ok(Region { base, size })
}

/// reg를 읽지 못한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RegError {
    Bad,
    NotTranslatable,
}

/// 버스 주소를 조상들의 ranges를 따라 CPU 물리 주소로 옮기는 함수입니다.
///
/// 깊이 `depth`의 노드가 가진 주소는 부모 버스의 주소 공간에 있습니다. 부모가
/// 루트가 될 때까지 각 버스의 ranges를 한 단계씩 적용합니다. ranges가 없는
/// 버스는 변환할 수 없고, 빈 ranges는 identity입니다. 구간 전체가 ranges 항목
/// 하나에 정확히 들어가야 하며, 둘 이상에 들어가는 모호한 경우도 거부합니다.
fn translate(levels: &Levels<'_>, depth: usize, addr: u64, size: u64) -> Result<u64, RegError> {
    let mut addr = addr;
    let mut k = depth - 1;
    while k >= 2 {
        let bus = &levels[k];
        let upper = &levels[k - 1];
        let ac = bus.addr_cells().map_err(|_| RegError::Bad)?.ok_or(RegError::Bad)?;
        let sc = bus.size_cells().map_err(|_| RegError::Bad)?.ok_or(RegError::Bad)?;
        let pac = match upper.addr_cells().map_err(|_| RegError::Bad)? {
            Some(v) => v,
            None if k - 1 == 1 => 2,
            None => return Err(RegError::Bad),
        };
        if !(1..=2).contains(&ac) || !(1..=2).contains(&sc) || !(1..=2).contains(&pac) {
            return Err(RegError::Bad);
        }
        let ranges = bus.ranges.ok_or(RegError::NotTranslatable)?;
        if ranges.is_empty() {
            if ac != pac {
                return Err(RegError::Bad);
            }
        } else {
            let entry = ((ac + pac + sc) * 4) as usize;
            if ranges.len() % entry != 0 {
                return Err(RegError::Bad);
            }
            let mut mapped: Option<u64> = None;
            for chunk in ranges.chunks_exact(entry) {
                let (c, rest) = chunk.split_at((ac * 4) as usize);
                let (p, l) = rest.split_at((pac * 4) as usize);
                let (child, parent, len) = (read_cells(c), read_cells(p), read_cells(l));
                let inside = addr >= child
                    && u128::from(addr) + u128::from(size) <= u128::from(child) + u128::from(len);
                if !inside {
                    continue;
                }
                if mapped.is_some() {
                    return Err(RegError::Bad);
                }
                let moved = u128::from(parent) + u128::from(addr - child);
                if moved + u128::from(size) > u128::from(u64::MAX) {
                    return Err(RegError::Bad);
                }
                mapped = Some(moved as u64);
            }
            addr = mapped.ok_or(RegError::NotTranslatable)?;
        }
        k -= 1;
    }
    Ok(addr)
}

/// 인터럽트 컨트롤러 노드를 해석하는 함수입니다. 컨트롤러와 그
/// #interrupt-cells 값을 함께 반환합니다.
fn read_intc(levels: &Levels<'_>, node: &Node<'_>, depth: usize) -> Result<(Intc, u32), DiscoverError> {
    if !usable(levels, node, depth) {
        return Err(DiscoverError::IntcDisabled);
    }
    if !node.has(P::InterruptController) {
        return Err(DiscoverError::IntcNotController);
    }
    if node.has(P::InterruptMap) {
        return Err(DiscoverError::IntcInterruptMap);
    }
    let v3 = node.compatible_has(b"arm,gic-v3");
    let v2 = [&b"arm,gic-400"[..], b"arm,cortex-a15-gic", b"arm,cortex-a7-gic"]
        .iter()
        .any(|c| node.compatible_has(c));
    let cells = node.u32(P::InterruptCells)?.ok_or(DiscoverError::BadIntcCells)?;
    let reg = |index: usize| {
        reg_entry(levels, node, depth, index).map_err(|e| match e {
            RegError::Bad => DiscoverError::BadIntcReg,
            RegError::NotTranslatable => DiscoverError::IntcNotTranslatable,
        })
    };

    match (v3, v2) {
        (true, false) => {
            if cells != 3 && cells != 4 {
                return Err(DiscoverError::BadIntcCells);
            }
            let dist = reg(0)?;
            // GICv3 프레임은 64KiB 단위이고 PIDR2가 프레임 끝(0xFFE8)에 있음
            if dist.base % 0x1_0000 != 0 || dist.size < 0x1_0000 {
                return Err(DiscoverError::BadIntcReg);
            }
            let count = node.u32(P::RedistRegions)?.unwrap_or(1) as usize;
            if !(1..=MAX_REDIST_REGIONS).contains(&count) {
                return Err(DiscoverError::BadRedistributor);
            }
            let mut redist = [Region::EMPTY; MAX_REDIST_REGIONS];
            for (i, slot) in redist.iter_mut().enumerate().take(count) {
                let r = reg(1 + i)?;
                // 리디스트리뷰터 하나는 64KiB 프레임 둘(RD_base, SGI_base)
                if r.base % 0x1_0000 != 0 || r.size < 0x2_0000 {
                    return Err(DiscoverError::BadRedistributor);
                }
                *slot = r;
            }
            let stride = match node.get(P::RedistStride) {
                None => 0,
                Some(v) if v.len() == 4 || v.len() == 8 => {
                    let s = read_cells(v);
                    if s < 0x2_0000 || s % 0x1_0000 != 0 {
                        return Err(DiscoverError::BadRedistributor);
                    }
                    s
                }
                Some(_) => return Err(DiscoverError::BadRedistributor),
            };
            Ok((
                Intc::GicV3 {
                    dist,
                    redist,
                    redist_count: count,
                    stride,
                },
                cells,
            ))
        }
        (false, true) => {
            if cells != 3 {
                return Err(DiscoverError::BadIntcCells);
            }
            // cpu-offset은 CPU 인터페이스가 코어별로 뱅크되지 않은 변종의 표시
            if node.has(P::CpuOffset) {
                return Err(DiscoverError::NonBankedGic);
            }
            let dist = reg(0)?;
            let cpu = reg(1)?;
            for r in [dist, cpu] {
                if r.base % 0x1000 != 0 || r.size < 0x1000 {
                    return Err(DiscoverError::BadIntcReg);
                }
            }
            Ok((Intc::GicV2 { dist, cpu }, cells))
        }
        _ => Err(DiscoverError::UnsupportedIntc),
    }
}

/// stdout-path가 가리키는 콘솔을 찾는 함수입니다.
///
/// 콘솔을 못 찾는 경우는 전부 안쪽 `Err(NoConsole)`이고, 바깥 `Err`는 DTB
/// 구조 위반뿐입니다.
fn find_console(fdt: &Fdt<'_>, stdout: Option<&[u8]>) -> Result<Result<Console, NoConsole>, BootError> {
    let Some(value) = stdout else {
        return Ok(Err(NoConsole::NoStdoutPath));
    };
    // "경로[:옵션]" 또는 "별칭[:옵션]" 형태의 문자열 하나
    let Some((&0, text)) = value.split_last() else {
        return Ok(Err(NoConsole::BadPath));
    };
    let target = text.split(|&b| b == b':').next().unwrap_or(&[]);
    if target.is_empty() || target.contains(&0) {
        return Ok(Err(NoConsole::BadPath));
    }
    let path = if target[0] == b'/' {
        target
    } else {
        // 별칭은 한 번만 풀고, 그 값은 반드시 전체 경로여야 함(별칭의 별칭 금지)
        match alias(fdt, target)? {
            None => return Ok(Err(NoConsole::UnknownAlias)),
            Some(v) => match v.split_last() {
                Some((&0, p)) if p.first() == Some(&b'/') && !p.contains(&0) => p,
                _ => return Ok(Err(NoConsole::BadPath)),
            },
        }
    };
    let components = path[1..].split(|&b| b == b'/');
    let count = components.clone().count();
    if count == 0 || count >= MAX_DEPTH || components.clone().any(|c| c.is_empty()) {
        return Ok(Err(NoConsole::BadPath));
    }

    // 경로의 모든 구성 요소가 노드 이름과 정확히 같아야 함(단위 주소 포함)
    let mut found: Option<Result<Console, NoConsole>> = None;
    let mut matches = 0usize;
    walk(fdt, |levels, node, depth| -> Result<Flow, BootError> {
        if depth == count + 1 {
            let mut names = levels[2..depth].iter().map(|l| l.name).chain(core::iter::once(node.name));
            if components.clone().all(|c| names.next() == Some(c)) {
                matches += 1;
                found = Some(read_console(levels, node, depth));
            }
        }
        Ok(Flow::Continue)
    })?;
    Ok(match (matches, found) {
        (1, Some(result)) => result,
        (0, _) => Err(NoConsole::NotFound),
        _ => Err(NoConsole::Ambiguous),
    })
}

fn read_console(levels: &Levels<'_>, node: &Node<'_>, depth: usize) -> Result<Console, NoConsole> {
    if !usable(levels, node, depth) {
        return Err(NoConsole::Disabled);
    }
    // 엔디언을 뒤집는 UART는 다루지 않음
    if node.has(P::BigEndian) || node.has(P::NativeEndian) {
        return Err(NoConsole::UnsupportedUart);
    }
    let reg = reg_entry(levels, node, depth, 0).map_err(|e| match e {
        RegError::Bad => NoConsole::BadProperties,
        RegError::NotTranslatable => NoConsole::NotTranslatable,
    })?;
    let small = |p: P, default: u32| match node.u32(p) {
        Ok(v) => Ok(v.unwrap_or(default)),
        Err(_) => Err(NoConsole::BadProperties),
    };

    let pl011 = node.compatible_has(b"arm,pl011");
    let sbsa = node.compatible_has(b"arm,sbsa-uart");
    let aux = node.compatible_has(b"brcm,bcm2835-aux-uart");
    let ns16550 = [&b"ns16550a"[..], b"ns16550", b"snps,dw-apb-uart"]
        .iter()
        .any(|c| node.compatible_has(c));

    let (kind, offset, span) = if pl011 {
        (ConsoleKind::Pl011, 0, PL011_SPAN)
    } else if sbsa {
        (ConsoleKind::SbsaUart, 0, PL011_SPAN)
    } else if aux || ns16550 {
        // 미니 UART는 레지스터 간격 4바이트, 32비트 접근으로 고정(Linux와 같음)
        let (shift, width) = if aux { (2, 4) } else { (small(P::RegShift, 0)?, small(P::RegIoWidth, 1)?) };
        if shift > 4 || (width != 1 && width != 4) {
            return Err(NoConsole::BadProperties);
        }
        let offset = if aux { 0 } else { small(P::RegOffset, 0)? };
        (
            ConsoleKind::Ns16550 {
                reg_shift: shift as u8,
                io_width: width as u8,
            },
            u64::from(offset),
            (NS16550_LSR << shift) + u64::from(width),
        )
    } else {
        return Err(NoConsole::UnsupportedUart);
    };

    // 디바이스 메모리에 대한 접근은 폭에 맞게 정렬돼야 함(어긋나면 폴트).
    // PL011 계열은 32비트 접근, 16550은 선언된 폭으로 0번과 5번 레지스터에 접근
    let width = match kind {
        ConsoleKind::Ns16550 { io_width, .. } => u64::from(io_width),
        _ => 4,
    };
    let base = reg.base.wrapping_add(offset);
    let lsr = match kind {
        ConsoleKind::Ns16550 { reg_shift, .. } => NS16550_LSR << reg_shift,
        _ => 0,
    };
    if base % width != 0 || lsr % width != 0 {
        return Err(NoConsole::BadProperties);
    }
    // 드라이버가 건드리는 범위가 선언된 구간 안에 있어야 함
    if offset.checked_add(span).is_none_or(|end| end > reg.size) {
        return Err(NoConsole::BadProperties);
    }
    Ok(Console {
        kind,
        base: reg.base + offset,
        span,
        reg,
    })
}

/// 플랫폼 기술서를 검증할 때 대조하는 메모리 배치입니다.
pub struct Layout<'a> {
    pub memory: &'a [MemRegion],
    /// 펌웨어 예약 구간(메모리로 선언되지 않은 곳에 있을 수도 있음)
    pub reserved: &'a [MemRegion],
    /// 커널 이미지가 차지하는 물리 범위
    pub image: Range<u64>,
    /// DTB가 차지하는 물리 범위
    pub dtb: Range<u64>,
    /// 변환 그래뉼 크기
    pub granule: u64,
    /// RAM이 넘으면 안 되는 물리 주소 상한(선형 별칭의 한계)
    pub ram_limit: u64,
    /// 디바이스가 넘으면 안 되는 물리 주소 상한(이 코어의 물리 주소 폭)
    pub device_limit: u64,
}

/// 플랫폼 기술서와 메모리 맵의 대조가 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidateError {
    MemoryAboveLimit,
    ImageOutsideMemory,
    DtbOutsideMemory,
    DeviceAboveLimit,
    DeviceOverlapsMemory,
    DevicesOverlap,
    ConsoleCrossesPage,
}

impl Platform {
    /// 이 플랫폼의 디바이스 구간들을 차례로 넘겨주는 함수입니다.
    fn device_regions(&self) -> impl Iterator<Item = Region> + '_ {
        let (first, second) = match &self.intc {
            Intc::GicV2 { dist, cpu } => (*dist, Some(*cpu)),
            Intc::GicV3 { dist, .. } => (*dist, None),
        };
        self.console
            .iter()
            .map(|c| c.reg)
            .chain(core::iter::once(first))
            .chain(second)
            .chain(self.intc.redist().iter().copied())
    }

    /// 발견한 디바이스와 메모리 맵이 서로 모순되지 않는지 검증하는 함수입니다.
    ///
    /// 디바이스 구간은 그래뉼 단위로 매핑되므로 그래뉼로 넓힌 범위가 어떤
    /// 메모리 구간과도 겹치면 안 됩니다. RAM을 디바이스로 매핑하면 사용자
    /// 공간이 쥔 프레임과 커널의 디바이스 매핑이 같은 물리 메모리를 가리키게
    /// 됩니다. 커널 이미지와 DTB는 선언된 메모리 안에 있어야 하며, 그래야
    /// 이 검사가 둘을 디바이스 구간으로부터도 지켜 줍니다.
    ///
    /// # Errors
    /// 모순이 있으면 `ValidateError`. 호출자는 부팅을 중단해야 합니다.
    pub fn validate(&self, layout: &Layout<'_>) -> Result<(), ValidateError> {
        let inside = |r: &Range<u64>| {
            layout
                .memory
                .iter()
                .any(|m| r.start >= m.base && r.end <= m.base + m.size)
        };
        if layout.memory.iter().any(|m| m.base + m.size > layout.ram_limit) {
            return Err(ValidateError::MemoryAboveLimit);
        }
        if !inside(&layout.image) {
            return Err(ValidateError::ImageOutsideMemory);
        }
        if !inside(&layout.dtb) {
            return Err(ValidateError::DtbOutsideMemory);
        }

        for (i, dev) in self.device_regions().enumerate() {
            check_device(layout, &dev)?;
            // 서로 다른 디바이스의 선언 구간끼리는 겹치면 안 됨(같은 그래뉼
            // 페이지를 나눠 쓰는 것은 허용)
            if self
                .device_regions()
                .skip(i + 1)
                .any(|o| o.base < dev.end() && dev.base < o.end())
            {
                return Err(ValidateError::DevicesOverlap);
            }
        }
        if let Some(c) = &self.console {
            c.check(layout)?;
        }
        Ok(())
    }
}

/// 디바이스 구간 하나를 메모리 배치와 대조하는 함수입니다. 그래뉼로 넓힌
/// 범위가 메모리나 예약 구간과 겹치면 안 되고 물리 주소 폭 안에 있어야 합니다.
fn check_device(layout: &Layout<'_>, dev: &Region) -> Result<(), ValidateError> {
    let g = layout.granule;
    if dev.end() > layout.device_limit {
        return Err(ValidateError::DeviceAboveLimit);
    }
    let start = dev.base & !(g - 1);
    let end = dev.end().div_ceil(g) * g;
    let hits = |m: &MemRegion| m.base < end && start < m.base + m.size;
    if layout.memory.iter().any(hits) || layout.reserved.iter().any(hits) {
        return Err(ValidateError::DeviceOverlapsMemory);
    }
    Ok(())
}

impl Console {
    /// 이 콘솔 하나만 메모리 배치와 대조하는 함수입니다. 플랫폼 전체의 발견이
    /// 실패했을 때 거부 사유를 내보낼 콘솔을 붙여도 되는지 판단하는 데 씁니다.
    ///
    /// # Errors
    /// [Platform::validate]의 콘솔 관련 항목과 같음
    pub fn check(&self, layout: &Layout<'_>) -> Result<(), ValidateError> {
        check_device(layout, &self.reg)?;
        let g = layout.granule;
        if self.base & !(g - 1) != (self.base + self.span - 1) & !(g - 1) {
            return Err(ValidateError::ConsoleCrossesPage);
        }
        Ok(())
    }
}

/// DTB에서 콘솔만 찾는 함수입니다.
///
/// 인터럽트 컨트롤러나 타이머 때문에 플랫폼 발견이 거부된 경우에도, 지원하는
/// 콘솔이 있으면 그 사유를 보여 줄 수 있게 하려는 용도입니다. 어떤 실패든
/// `None`입니다.
pub fn discover_console(dtb_phys: usize, read_offset: usize) -> Option<Console> {
    let read_base = dtb_phys.checked_add(read_offset)?;
    let span = dtb_span(read_base).ok()?;
    // SAFETY: discover와 같은 전제
    let blob = unsafe { core::slice::from_raw_parts(span.start as *const u8, span.end - span.start) };
    console_of(blob)
}

pub(crate) fn console_of(blob: &[u8]) -> Option<Console> {
    let fdt = Fdt::new(blob).ok()?;
    let mut stdout: Option<&[u8]> = None;
    walk(&fdt, |_, node, depth| -> Result<Flow, BootError> {
        if depth == 2 && node.name == b"chosen" {
            stdout = node.get(P::StdoutPath).or(node.get(P::LinuxStdoutPath));
        }
        Ok(Flow::Continue)
    })
    .ok()?;
    find_console(&fdt, stdout).ok()?.ok()
}

impl fmt::Display for Platform {
    /// 플랫폼을 한 줄의 정해진 형식으로 적습니다. 부트 로그에 찍히는 형식이자
    /// 고정 모드가 대조하는 형식이므로 같은 플랫폼은 항상 같은 글자열이 됩니다.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.console {
            None => write!(f, "con=none")?,
            Some(c) => match c.kind {
                ConsoleKind::Pl011 => write!(f, "con=pl011@{:#x}", c.base)?,
                ConsoleKind::SbsaUart => write!(f, "con=sbsa@{:#x}", c.base)?,
                ConsoleKind::Ns16550 { reg_shift, io_width } => {
                    write!(f, "con=ns16550@{:#x},s{reg_shift},w{io_width}", c.base)?;
                }
            },
        }
        match &self.intc {
            Intc::GicV2 { dist, cpu } => write!(f, " gic=v2@{:#x} cpu={:#x}", dist.base, cpu.base)?,
            Intc::GicV3 { dist, stride, .. } => {
                write!(f, " gic=v3@{:#x} rd=", dist.base)?;
                for (i, r) in self.intc.redist().iter().enumerate() {
                    let sep = if i == 0 { "" } else { ";" };
                    write!(f, "{sep}{:#x}+{:#x}", r.base, r.size)?;
                }
                if *stride != 0 {
                    write!(f, " stride={stride:#x}")?;
                }
            }
        }
        write!(f, " tmr={}", self.timer.intid)?;
        if self.timer.clock_hz != 0 {
            write!(f, " hz={}", self.timer.clock_hz)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
