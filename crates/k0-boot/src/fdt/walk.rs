//! FDT 구조 블록을 검증하며 훑는 워커 모듈입니다.
//!
//! # Features
//! 헤더 검증을 마친 뷰([Fdt]) 위에서 토큰을 하나씩 읽는 커서([Cursor])와,
//! 노드 하나의 프로퍼티가 모두 모였을 때 조상 문맥과 함께 한 번씩 알려 주는
//! 순회([walk])를 제공합니다. 힙 없이 깊이 상한만큼의 고정 스택을 씁니다.
//! 명세가 금지하는 형태(자식 노드 뒤의 프로퍼티, 두 번째 루트, 커널이
//! 해석하는 프로퍼티의 중복)는 해석하지 않고 거부합니다. 같은 blob을 다른
//! 파서가 다르게 읽을 여지를 없애기 위함입니다.
//!
//! # Errors
//! 형식 위반은 전부 `BootError`이며 어떤 경우에도 blob 바깥을 읽지 않습니다.

use super::BootError;

/// 노드 중첩 깊이 상한 (루트가 1)
pub(crate) const MAX_DEPTH: usize = 32;

/// 프로퍼티 이름 길이 상한입니다. 명세의 상한은 31자이고 실제 트리에서 본 가장
/// 긴 이름도 40자를 넘지 않습니다. 이름의 끝을 찾는 탐색을 이 길이로 묶어,
/// 수많은 프로퍼티가 NUL 없는 긴 구간을 가리키는 blob으로도 해석 시간이
/// 입력 크기에 비례하는 선을 넘지 않게 합니다.
const MAX_PROP_NAME: usize = 64;

const FDT_MAGIC: u32 = 0xd00d_feed;
const SUPPORTED_VERSION: u32 = 17;

const FDT_BEGIN_NODE: u32 = 0x1;
const FDT_END_NODE: u32 = 0x2;
const FDT_PROP: u32 = 0x3;
const FDT_NOP: u32 = 0x4;
const FDT_END: u32 = 0x9;

pub fn be32(blob: &[u8], off: usize) -> Result<u32, BootError> {
    let b = blob.get(off..off.wrapping_add(4)).ok_or(BootError::Truncated)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn be64(blob: &[u8], off: usize) -> Result<u64, BootError> {
    let b = blob.get(off..off.wrapping_add(8)).ok_or(BootError::Truncated)?;
    Ok(u64::from_be_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

fn align4(off: usize) -> Result<usize, BootError> {
    off.checked_add(3).map(|v| v & !3).ok_or(BootError::Truncated)
}

/// 셀 열(big-endian u32 묶음)을 정수 하나로 읽는 함수입니다. 호출자가 셀 수를
/// 2 이하로 제한합니다.
pub fn read_cells(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

/// 헤더 검증을 마친 DTB 뷰입니다.
///
/// 구조 블록과 문자열 블록의 범위가 blob 안에 있음이 보장됩니다.
#[derive(Clone, Copy)]
pub struct Fdt<'a> {
    blob: &'a [u8],
    struct_start: usize,
    struct_end: usize,
    strings_start: usize,
    strings_end: usize,
    rsvmap: usize,
}

impl<'a> Fdt<'a> {
    /// blob의 헤더를 검증해 뷰를 만드는 함수입니다.
    ///
    /// # Errors
    /// 매직, 버전, 블록 오프셋과 크기가 형식에 맞지 않으면 `BootError`
    pub fn new(blob: &'a [u8]) -> Result<Self, BootError> {
        if be32(blob, 0)? != FDT_MAGIC {
            return Err(BootError::BadMagic);
        }
        let off_struct = be32(blob, 8)? as usize;
        let off_strings = be32(blob, 12)? as usize;
        let off_rsvmap = be32(blob, 16)? as usize;
        let version = be32(blob, 20)?;
        let last_comp_version = be32(blob, 24)?;
        let size_strings = be32(blob, 32)? as usize;
        let size_struct = be32(blob, 36)? as usize;

        if version < SUPPORTED_VERSION || last_comp_version > SUPPORTED_VERSION {
            return Err(BootError::UnsupportedVersion);
        }
        // 구조 블록은 4바이트 토큰 열이라 시작과 크기 모두 4의 배수여야 함
        // (크기가 어긋나면 마지막 토큰이 블록 끝을 넘어 걸치게 됨)
        if off_struct % 4 != 0 || size_struct % 4 != 0 || off_rsvmap % 8 != 0 {
            return Err(BootError::BadHeader);
        }
        let struct_end = off_struct
            .checked_add(size_struct)
            .filter(|&e| e <= blob.len())
            .ok_or(BootError::BadHeader)?;
        let strings_end = off_strings
            .checked_add(size_strings)
            .filter(|&e| e <= blob.len())
            .ok_or(BootError::BadHeader)?;

        Ok(Self {
            blob,
            struct_start: off_struct,
            struct_end,
            strings_start: off_strings,
            strings_end,
            rsvmap: off_rsvmap,
        })
    }

    /// memreserve 블록의 (주소, 크기) 쌍을 차례로 넘겨주는 함수입니다.
    ///
    /// 블록은 (0, 0) 쌍으로 끝납니다. 크기가 0인 쌍은 건너뜁니다.
    ///
    /// # Errors
    /// 종결 쌍 없이 blob이 끝나면 `Truncated`, 콜백의 오류는 그대로 전달
    pub fn memreserve(
        &self,
        mut f: impl FnMut(u64, u64) -> Result<(), BootError>,
    ) -> Result<(), BootError> {
        let mut off = self.rsvmap;
        loop {
            let base = be64(self.blob, off)?;
            let size = be64(self.blob, off.checked_add(8).ok_or(BootError::Truncated)?)?;
            off = off.checked_add(16).ok_or(BootError::Truncated)?;
            if base == 0 && size == 0 {
                return Ok(());
            }
            if size != 0 {
                f(base, size)?;
            }
        }
    }

    /// 구조 블록의 처음을 가리키는 커서를 만드는 함수입니다.
    pub fn cursor(&self) -> Cursor<'a> {
        Cursor {
            fdt: *self,
            off: self.struct_start,
            depth: 0,
            child_seen: [false; MAX_DEPTH + 1],
            root_done: false,
        }
    }

    fn prop_name(&self, name_off: usize) -> Result<&'a [u8], BootError> {
        let start = self
            .strings_start
            .checked_add(name_off)
            .filter(|&s| s < self.strings_end)
            .ok_or(BootError::BadString)?;
        let limit = self.strings_end.min(start + MAX_PROP_NAME + 1);
        let rel = self.blob[start..limit]
            .iter()
            .position(|&b| b == 0)
            .ok_or(BootError::BadString)?;
        Ok(&self.blob[start..start + rel])
    }
}

/// 구조 블록의 토큰 하나를 나타내는 열거형입니다.
pub enum Token<'a> {
    Begin { name: &'a [u8] },
    Prop { name: &'a [u8], value: &'a [u8] },
    End,
}

/// 구조 블록을 토큰 단위로 읽으며 중첩 규칙을 강제하는 커서입니다.
pub struct Cursor<'a> {
    fdt: Fdt<'a>,
    off: usize,
    depth: usize,
    /// 각 깊이의 열린 노드가 이미 자식을 가졌는지 여부
    child_seen: [bool; MAX_DEPTH + 1],
    root_done: bool,
}

impl<'a> Cursor<'a> {
    /// 직전 토큰까지 읽은 뒤의 열린 노드 수를 주는 함수입니다.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// 다음 토큰을 읽는 함수입니다. FDT_END에서 `None`을 반환합니다.
    ///
    /// # Errors
    /// 알 수 없는 토큰, 블록을 벗어나는 길이, 깊이 초과, 이름 있는 루트,
    /// 두 번째 루트, 자식 뒤의 프로퍼티, 닫히지 않은 노드는 `BootError`
    pub fn next(&mut self) -> Result<Option<Token<'a>>, BootError> {
        let blob = self.fdt.blob;
        let struct_end = self.fdt.struct_end;
        loop {
            if self.off >= struct_end {
                return Err(BootError::BadStructure);
            }
            let token = be32(blob, self.off)?;
            self.off += 4;

            match token {
                FDT_BEGIN_NODE => {
                    if self.root_done {
                        return Err(BootError::BadStructure);
                    }
                    if self.depth == MAX_DEPTH {
                        return Err(BootError::TooDeep);
                    }
                    let rel = blob
                        .get(self.off..struct_end)
                        .ok_or(BootError::BadStructure)?
                        .iter()
                        .position(|&b| b == 0)
                        .ok_or(BootError::BadStructure)?;
                    let name = &blob[self.off..self.off + rel];
                    if self.depth == 0 && !name.is_empty() {
                        return Err(BootError::BadStructure);
                    }
                    self.off = align4(self.off + rel + 1)?;
                    self.child_seen[self.depth] = true;
                    self.depth += 1;
                    self.child_seen[self.depth] = false;
                    return Ok(Some(Token::Begin { name }));
                }
                FDT_END_NODE => {
                    if self.depth == 0 {
                        return Err(BootError::BadStructure);
                    }
                    self.depth -= 1;
                    if self.depth == 0 {
                        self.root_done = true;
                    }
                    return Ok(Some(Token::End));
                }
                FDT_PROP => {
                    // 명세는 한 노드의 프로퍼티가 전부 자식 노드보다 앞에 오도록
                    // 요구함. 뒤에 온 프로퍼티를 읽는 파서와 무시하는 파서가
                    // 같은 blob을 다르게 해석하게 되므로 거부함
                    if self.depth == 0 || self.child_seen[self.depth] {
                        return Err(BootError::BadStructure);
                    }
                    let len = be32(blob, self.off)? as usize;
                    let name_off = be32(blob, self.off + 4)? as usize;
                    let val_start = self.off + 8;
                    let val_end = val_start
                        .checked_add(len)
                        .filter(|&e| e <= struct_end)
                        .ok_or(BootError::Truncated)?;
                    let name = self.fdt.prop_name(name_off)?;
                    self.off = align4(val_end)?;
                    return Ok(Some(Token::Prop {
                        name,
                        value: &blob[val_start..val_end],
                    }));
                }
                FDT_NOP => {}
                FDT_END => {
                    if self.depth != 0 {
                        return Err(BootError::BadStructure);
                    }
                    return Ok(None);
                }
                _ => return Err(BootError::BadStructure),
            }
        }
    }
}

/// 커널이 해석하는 프로퍼티의 식별자입니다. 여기 없는 프로퍼티는 읽지 않습니다.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum P {
    Compatible,
    Status,
    Reg,
    Ranges,
    AddressCells,
    SizeCells,
    Phandle,
    LinuxPhandle,
    InterruptParent,
    Interrupts,
    InterruptNames,
    InterruptCells,
    InterruptController,
    InterruptMap,
    InterruptsExtended,
    RedistRegions,
    RedistStride,
    ClockFrequency,
    RegShift,
    RegIoWidth,
    RegOffset,
    BigEndian,
    NativeEndian,
    CpuOffset,
    DeviceType,
    StdoutPath,
    LinuxStdoutPath,
    RngSeed,
    KaslrSeed,
    UsableMemoryRange,
    UsableMemory,
    InitrdStart,
    InitrdEnd,
    ErratumA008585,
    Erratum161010101,
    ErratumUnknown1,
}

const PROP_COUNT: usize = 36;

const PROP_NAMES: [&[u8]; PROP_COUNT] = [
    b"compatible",
    b"status",
    b"reg",
    b"ranges",
    b"#address-cells",
    b"#size-cells",
    b"phandle",
    b"linux,phandle",
    b"interrupt-parent",
    b"interrupts",
    b"interrupt-names",
    b"#interrupt-cells",
    b"interrupt-controller",
    b"interrupt-map",
    b"interrupts-extended",
    b"#redistributor-regions",
    b"redistributor-stride",
    b"clock-frequency",
    b"reg-shift",
    b"reg-io-width",
    b"reg-offset",
    b"big-endian",
    b"native-endian",
    b"cpu-offset",
    b"device_type",
    b"stdout-path",
    b"linux,stdout-path",
    b"rng-seed",
    b"kaslr-seed",
    b"linux,usable-memory-range",
    b"linux,usable-memory",
    b"linux,initrd-start",
    b"linux,initrd-end",
    b"fsl,erratum-a008585",
    b"hisilicon,erratum-161010101",
    b"allwinner,erratum-unknown1",
];

/// 프로퍼티가 모두 모인 노드 하나를 나타내는 구조체입니다.
#[derive(Clone, Copy)]
pub(crate) struct Node<'a> {
    pub name: &'a [u8],
    vals: [Option<&'a [u8]>; PROP_COUNT],
}

impl<'a> Node<'a> {
    fn new(name: &'a [u8]) -> Self {
        Self {
            name,
            vals: [None; PROP_COUNT],
        }
    }

    pub(crate) fn get(&self, p: P) -> Option<&'a [u8]> {
        self.vals[p as usize]
    }

    pub(crate) fn has(&self, p: P) -> bool {
        self.vals[p as usize].is_some()
    }

    /// 셀 하나짜리 프로퍼티를 읽는 함수입니다.
    ///
    /// # Errors
    /// 프로퍼티가 있는데 길이가 4바이트가 아니면 `UnsupportedCells`
    pub(crate) fn u32(&self, p: P) -> Result<Option<u32>, BootError> {
        cell(self.get(p))
    }

    /// 이 노드가 사용 가능한 상태인지 판정하는 함수입니다.
    ///
    /// status가 없거나 "okay" / "ok"일 때만 참입니다. 그 밖의 값("disabled",
    /// "reserved", "fail" 등과 알 수 없는 문자열)은 전부 사용 불가입니다.
    pub(crate) fn available(&self) -> bool {
        match self.get(P::Status) {
            None => true,
            Some(v) => v == b"okay\0" || v == b"ok\0",
        }
    }

    /// compatible 문자열 목록에 `want`가 있는지 확인하는 함수입니다.
    pub(crate) fn compatible_has(&self, want: &[u8]) -> bool {
        match self.get(P::Compatible) {
            None => false,
            Some(list) => strings(list).any(|s| s == want),
        }
    }

    /// 이 노드의 phandle을 주는 함수입니다. (phandle이 없으면 linux,phandle)
    pub(crate) fn phandle(&self) -> Result<Option<u32>, BootError> {
        match self.u32(P::Phandle)? {
            Some(v) => Ok(Some(v)),
            None => self.u32(P::LinuxPhandle),
        }
    }
}

/// NUL로 끝나는 문자열이 이어진 프로퍼티 값을 문자열 단위로 나누는 함수입니다.
/// 마지막 바이트가 NUL이 아닌 값은 형식 위반이라 문자열을 하나도 내지 않습니다.
pub(crate) fn strings(list: &[u8]) -> impl Iterator<Item = &[u8]> {
    let body = match list.split_last() {
        Some((&0, body)) => Some(body),
        _ => None,
    };
    body.into_iter().flat_map(|b| b.split(|&c| c == 0))
}

/// 조상 노드 하나가 자손에게 주는 문맥을 나타내는 구조체입니다.
///
/// 셀과 인터럽트 부모는 원본 바이트로 들고 있다가 쓰는 쪽에서 해석합니다.
/// 커널이 보지 않는 가지의 형식 오류가 메모리 맵 해석까지 막지 않게 하기
/// 위함입니다.
#[derive(Clone, Copy)]
pub(crate) struct Level<'a> {
    pub name: &'a [u8],
    addr_cells: Option<&'a [u8]>,
    size_cells: Option<&'a [u8]>,
    /// None은 ranges 없음(변환 불가), 빈 슬라이스는 identity
    pub ranges: Option<&'a [u8]>,
    pub available: bool,
    interrupt_parent: Option<&'a [u8]>,
    pub has_interrupt_cells: bool,
}

fn cell(value: Option<&[u8]>) -> Result<Option<u32>, BootError> {
    match value {
        None => Ok(None),
        Some(v) if v.len() == 4 => Ok(Some(u32::from_be_bytes([v[0], v[1], v[2], v[3]]))),
        Some(_) => Err(BootError::UnsupportedCells),
    }
}

impl<'a> Level<'a> {
    const EMPTY: Level<'static> = Level {
        name: &[],
        addr_cells: None,
        size_cells: None,
        ranges: None,
        available: true,
        interrupt_parent: None,
        has_interrupt_cells: false,
    };

    fn of(node: &Node<'a>) -> Self {
        Self {
            name: node.name,
            addr_cells: node.get(P::AddressCells),
            size_cells: node.get(P::SizeCells),
            ranges: node.get(P::Ranges),
            available: node.available(),
            interrupt_parent: node.get(P::InterruptParent),
            has_interrupt_cells: node.has(P::InterruptCells),
        }
    }

    /// 이 노드의 자식 주소 공간 셀 수(#address-cells)를 주는 함수입니다.
    ///
    /// # Errors
    /// 프로퍼티가 있는데 길이가 4바이트가 아니면 `UnsupportedCells`
    pub(crate) fn addr_cells(&self) -> Result<Option<u32>, BootError> {
        cell(self.addr_cells)
    }

    /// 이 노드의 자식 크기 셀 수(#size-cells)를 주는 함수입니다.
    ///
    /// # Errors
    /// 프로퍼티가 있는데 길이가 4바이트가 아니면 `UnsupportedCells`
    pub(crate) fn size_cells(&self) -> Result<Option<u32>, BootError> {
        cell(self.size_cells)
    }

    /// 이 노드의 interrupt-parent phandle을 주는 함수입니다.
    ///
    /// # Errors
    /// 프로퍼티가 있는데 길이가 4바이트가 아니면 `UnsupportedCells`
    pub(crate) fn interrupt_parent(&self) -> Result<Option<u32>, BootError> {
        cell(self.interrupt_parent)
    }
}

/// 조상 문맥 스택의 스냅샷입니다. 인덱스가 곧 깊이이며 0번은 쓰지 않습니다.
pub(crate) type Levels<'a> = [Level<'a>; MAX_DEPTH + 1];

/// 순회를 계속할지 여부를 나타내는 열거형입니다.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    Continue,
    Stop,
}

/// 트리의 모든 노드를 한 번씩 방문하는 함수입니다.
///
/// 방문 시점은 그 노드의 프로퍼티가 모두 모인 직후(첫 자식 직전 또는 노드
/// 끝)입니다. 방문자는 `levels[1..depth]`로 조상들의 문맥을 보고, `node`로
/// 방문 중인 노드를 봅니다. `Flow::Stop`은 나머지 구조 검증까지 건너뛰므로,
/// 구조 전체가 이미 검증된 뒤의 재순회에서만 써야 합니다.
///
/// # Errors
/// 구조 위반과 해석 대상 프로퍼티의 중복은 `BootError`(E로 변환), 방문자의
/// 오류는 그대로 전달
pub(crate) fn walk<'a, E: From<BootError>>(
    fdt: &Fdt<'a>,
    mut visit: impl FnMut(&Levels<'a>, &Node<'a>, usize) -> Result<Flow, E>,
) -> Result<(), E> {
    let mut levels: Levels<'a> = [Level::EMPTY; MAX_DEPTH + 1];
    let mut cur: Option<Node<'a>> = None;
    let mut cursor = fdt.cursor();

    while let Some(token) = cursor.next()? {
        match token {
            Token::Begin { name } => {
                // 부모의 프로퍼티가 여기서 확정됨
                if let Some(parent) = cur.take() {
                    let depth = cursor.depth() - 1;
                    if visit(&levels, &parent, depth)? == Flow::Stop {
                        return Ok(());
                    }
                    levels[depth] = Level::of(&parent);
                }
                cur = Some(Node::new(name));
            }
            Token::Prop { name, value } => {
                // 커서가 자식 뒤의 프로퍼티를 거부하므로 열린 노드가 반드시 있음
                let node = cur.as_mut().ok_or(BootError::BadStructure)?;
                if let Some(i) = PROP_NAMES.iter().position(|&n| n == name) {
                    if node.vals[i].is_some() {
                        return Err(BootError::DuplicateProperty.into());
                    }
                    node.vals[i] = Some(value);
                }
            }
            Token::End => {
                // 자식이 없던 노드는 끝에서 확정됨
                if let Some(leaf) = cur.take() {
                    let depth = cursor.depth() + 1;
                    if visit(&levels, &leaf, depth)? == Flow::Stop {
                        return Ok(());
                    }
                }
            }
        }
    }
    Ok(())
}

/// `/aliases` 노드에서 별칭 하나의 값을 찾는 함수입니다.
///
/// 별칭 이름은 임의의 프로퍼티 이름이라 [walk]의 고정 표로는 읽을 수 없어
/// 커서로 직접 훑습니다.
///
/// # Errors
/// 구조 위반 시 `BootError`
pub(crate) fn alias<'a>(fdt: &Fdt<'a>, want: &[u8]) -> Result<Option<&'a [u8]>, BootError> {
    let mut cursor = fdt.cursor();
    let mut in_aliases = false;
    let mut found = None;
    while let Some(token) = cursor.next()? {
        match token {
            Token::Begin { name, .. } => {
                in_aliases = cursor.depth() == 2 && name == b"aliases";
            }
            Token::Prop { name, value } => {
                if in_aliases && cursor.depth() == 2 && name == want && found.is_none() {
                    found = Some(value);
                }
            }
            Token::End => {
                if cursor.depth() < 2 {
                    in_aliases = false;
                }
            }
        }
    }
    Ok(found)
}
