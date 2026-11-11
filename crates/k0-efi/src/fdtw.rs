//! DTB를 쓰는 모듈입니다.
//!
//! # Features
//! 호출자가 준 버퍼 안에 DTB를 조립하는 작성기와, 펌웨어가 준 DTB를 메모리
//! 기술만 바꿔 다시 쓰는 재작성기가 들어 있습니다. 재작성기는 입력을 커널의
//! 워커로 읽어 토큰 단위로 다시 내보내므로, 커널이 구조를 거부할 입력은
//! 여기서도 거부됩니다. 출력의 문자열 블록은 새로 만듭니다.
//!
//! 재작성이 바꾸는 것은 다음뿐입니다.
//! - 루트 바로 아래의 `memory`, `memory@...`, `reserved-memory` 노드를 뺌
//! - memory reservation block을 비움
//! - `/chosen`의 `linux,initrd-start`, `linux,initrd-end`,
//!   `linux,usable-memory-range`를 빼고 `rng-seed`를 새 값으로 바꿈
//! - 계획한 구간을 담은 메모리 노드 하나를 루트의 마지막 자식으로 더함

use k0_boot::{BootError, Fdt, MemRegion, Token, MAX_MEM_REGIONS};

const MAGIC: u32 = 0xd00d_feed;
const HEADER: usize = 40;
const RSV_ENTRY: usize = 16;
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const END: u32 = 9;

/// 펌웨어의 `rng-seed`에서 가져오는 최대 바이트 수입니다.
pub const FIRMWARE_SEED_MAX: usize = 32;

/// 로더가 더하는 엔트로피의 최대 바이트 수입니다.
pub const LOADER_SEED_MAX: usize = 32;

/// 작성기가 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// 버퍼가 모자람
    NoSpace,
    /// 이름에 0 바이트가 들어 있음
    BadName,
    /// 닫히지 않은 노드가 있거나 열지 않은 노드를 닫음
    Unbalanced,
}

/// 호출자의 버퍼에 DTB를 조립하는 작성기입니다.
///
/// 구조 블록은 앞에서부터 쓰고, 문자열 블록은 버퍼 끝의 임시 자리에 모았다가
/// 마칠 때 구조 블록 바로 뒤로 옮깁니다. 같은 프로퍼티 이름은 한 번만
/// 저장합니다.
pub struct Writer<'a> {
    buf: &'a mut [u8],
    struct_start: usize,
    pos: usize,
    str_base: usize,
    str_len: usize,
    depth: usize,
}

impl<'a> Writer<'a> {
    /// 버퍼를 비우고 작성기를 만드는 함수입니다.
    ///
    /// # Arguments
    /// `buf` - 출력 버퍼, 전부 0으로 지운 뒤 씀
    /// `reserved` - memory reservation block에 넣을 구간
    /// `strings_cap` - 문자열 블록에 필요한 최대 바이트 수
    ///
    /// # Errors
    /// 버퍼가 헤더와 문자열 자리도 담지 못하면 `NoSpace`
    pub fn new(buf: &'a mut [u8], reserved: &[MemRegion], strings_cap: usize) -> Result<Self, WriteError> {
        buf.fill(0);
        let rsv_bytes = reserved
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(RSV_ENTRY))
            .ok_or(WriteError::NoSpace)?;
        let struct_start = HEADER.checked_add(rsv_bytes).ok_or(WriteError::NoSpace)?;
        let str_base = buf.len().checked_sub(strings_cap).ok_or(WriteError::NoSpace)?;
        // 구조 블록에는 적어도 끝 토큰 하나가 들어가야 함
        if struct_start.checked_add(4).is_none_or(|e| e > str_base) {
            return Err(WriteError::NoSpace);
        }
        for (i, r) in reserved.iter().enumerate() {
            let off = HEADER + i * RSV_ENTRY;
            buf[off..off + 8].copy_from_slice(&r.base.to_be_bytes());
            buf[off + 8..off + 16].copy_from_slice(&r.size.to_be_bytes());
        }
        Ok(Self { buf, struct_start, pos: struct_start, str_base, str_len: 0, depth: 0 })
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), WriteError> {
        let end = self.pos.checked_add(bytes.len()).ok_or(WriteError::NoSpace)?;
        if end > self.str_base {
            return Err(WriteError::NoSpace);
        }
        self.buf[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }

    fn put_u32(&mut self, v: u32) -> Result<(), WriteError> {
        self.put(&v.to_be_bytes())
    }

    /// 4바이트 경계까지 0으로 채우는 함수입니다. 버퍼는 이미 0이므로 위치만 옮김
    fn pad(&mut self) -> Result<(), WriteError> {
        let end = self.pos.checked_add(3).ok_or(WriteError::NoSpace)? & !3;
        if end > self.str_base {
            return Err(WriteError::NoSpace);
        }
        self.pos = end;
        Ok(())
    }

    fn intern(&mut self, name: &[u8]) -> Result<u32, WriteError> {
        if name.contains(&0) {
            return Err(WriteError::BadName);
        }
        let table = &self.buf[self.str_base..self.str_base + self.str_len];
        let mut off = 0;
        for existing in table.split(|&b| b == 0) {
            if off >= table.len() {
                break;
            }
            if existing == name {
                return u32::try_from(off).map_err(|_| WriteError::NoSpace);
            }
            off += existing.len() + 1;
        }
        let start = self.str_base + self.str_len;
        let end = start
            .checked_add(name.len())
            .and_then(|e| e.checked_add(1))
            .ok_or(WriteError::NoSpace)?;
        if end > self.buf.len() {
            return Err(WriteError::NoSpace);
        }
        self.buf[start..end - 1].copy_from_slice(name);
        self.buf[end - 1] = 0;
        let at = self.str_len;
        self.str_len = end - self.str_base;
        u32::try_from(at).map_err(|_| WriteError::NoSpace)
    }

    /// 노드를 여는 함수입니다.
    ///
    /// # Arguments
    /// `name` - 노드 이름, 루트는 빈 이름
    pub fn begin(&mut self, name: &[u8]) -> Result<(), WriteError> {
        if name.contains(&0) {
            return Err(WriteError::BadName);
        }
        self.put_u32(BEGIN_NODE)?;
        self.put(name)?;
        self.put(&[0])?;
        self.pad()?;
        self.depth += 1;
        Ok(())
    }

    /// 프로퍼티 하나를 쓰는 함수입니다.
    ///
    /// # Arguments
    /// `name` - 프로퍼티 이름
    /// `value` - 값 바이트열
    pub fn prop(&mut self, name: &[u8], value: &[u8]) -> Result<(), WriteError> {
        if self.depth == 0 {
            return Err(WriteError::Unbalanced);
        }
        let len = u32::try_from(value.len()).map_err(|_| WriteError::NoSpace)?;
        let name_off = self.intern(name)?;
        self.put_u32(PROP)?;
        self.put_u32(len)?;
        self.put_u32(name_off)?;
        self.put(value)?;
        self.pad()
    }

    /// 가장 최근에 연 노드를 닫는 함수입니다.
    pub fn end(&mut self) -> Result<(), WriteError> {
        if self.depth == 0 {
            return Err(WriteError::Unbalanced);
        }
        self.put_u32(END_NODE)?;
        self.depth -= 1;
        Ok(())
    }

    /// 끝 토큰과 헤더를 쓰고 DTB의 전체 길이를 주는 함수입니다.
    ///
    /// # Arguments
    /// `boot_cpuid` - 헤더의 boot_cpuid_phys 값
    ///
    /// # Errors
    /// 닫히지 않은 노드가 있으면 `Unbalanced`
    pub fn finish(mut self, boot_cpuid: u32) -> Result<usize, WriteError> {
        if self.depth != 0 {
            return Err(WriteError::Unbalanced);
        }
        self.put_u32(END)?;
        let struct_size = self.pos - self.struct_start;
        let strings_off = self.pos;
        self.buf
            .copy_within(self.str_base..self.str_base + self.str_len, strings_off);
        let total = strings_off + self.str_len;
        // 임시 자리에 남은 문자열 조각을 지움
        self.buf[total..].fill(0);

        let field = |v: usize| u32::try_from(v).map_err(|_| WriteError::NoSpace);
        let header = [
            MAGIC,
            field(total)?,
            field(self.struct_start)?,
            field(strings_off)?,
            HEADER as u32,
            17,
            16,
            boot_cpuid,
            field(self.str_len)?,
            field(struct_size)?,
        ];
        for (i, v) in header.iter().enumerate() {
            self.buf[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        Ok(total)
    }
}

/// 재작성이 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewriteError {
    /// 입력 DTB를 커널의 워커가 거부함
    Input(BootError),
    /// 루트 아래에 `chosen` 노드가 둘 이상임
    DuplicateChosen,
    /// 루트의 셀 폭이 1이나 2가 아님
    UnsupportedCells,
    /// 메모리 구간이 없거나 8개를 넘음
    BadRegionCount,
    /// 구간의 주소나 크기가 루트의 셀 폭에 들어가지 않음
    RegionTooLarge,
    /// 출력 버퍼에 쓰지 못함
    Output(WriteError),
}

impl From<BootError> for RewriteError {
    fn from(e: BootError) -> Self {
        RewriteError::Input(e)
    }
}

impl From<WriteError> for RewriteError {
    fn from(e: WriteError) -> Self {
        RewriteError::Output(e)
    }
}

/// 재작성에서 바꿀 내용입니다.
pub struct Edits<'a> {
    /// 새 메모리 노드에 담을 구간(1개 이상 8개 이하)
    pub memory: &'a [MemRegion],
    /// 펌웨어의 `rng-seed` 뒤에 붙일 엔트로피, 없으면 빈 슬라이스
    pub seed: &'a [u8],
}

fn cell_value(bytes: &[u8]) -> Result<u32, RewriteError> {
    let b: [u8; 4] = bytes.try_into().map_err(|_| RewriteError::UnsupportedCells)?;
    match u32::from_be_bytes(b) {
        v @ 1..=2 => Ok(v),
        _ => Err(RewriteError::UnsupportedCells),
    }
}

/// 루트 노드의 `#address-cells`와 `#size-cells`를 읽는 함수입니다. 없으면
/// 커널과 같은 기본값(2와 1)을 씁니다.
///
/// # Arguments
/// `fdt` - 헤더 검증을 마친 DTB
///
/// # Errors
/// 구조 위반은 `Input`, 셀 폭이 1이나 2가 아니면 `UnsupportedCells`
pub fn root_cells(fdt: &Fdt<'_>) -> Result<(u32, u32), RewriteError> {
    let (mut addr, mut size) = (2, 1);
    let mut cur = fdt.cursor();
    while let Some(tok) = cur.next()? {
        match tok {
            Token::Prop { name, value } if cur.depth() == 1 => match name {
                b"#address-cells" => addr = cell_value(value)?,
                b"#size-cells" => size = cell_value(value)?,
                _ => {}
            },
            // 루트의 프로퍼티는 첫 자식보다 앞에 있으므로 여기서 끝냄
            Token::Begin { .. } if cur.depth() >= 2 => break,
            Token::End if cur.depth() == 0 => break,
            _ => {}
        }
    }
    Ok((addr, size))
}

/// 엔트로피가 지나간 스택 버퍼를 지우는 함수입니다. 이 크레이트는 unsafe를 쓰지
/// 않으므로, 지우는 쓰기가 최적화로 사라지지 않게 값을 쓰이는 것처럼 보이게
/// 합니다(최선 노력).
fn wipe(buf: &mut [u8]) {
    buf.fill(0);
    core::hint::black_box(buf);
}

fn is_memory_name(name: &[u8]) -> bool {
    name == b"memory" || name.starts_with(b"memory@")
}

fn push_cells(out: &mut [u8], at: &mut usize, cells: u32, v: u64) -> Result<(), RewriteError> {
    if cells == 1 {
        let v = u32::try_from(v).map_err(|_| RewriteError::RegionTooLarge)?;
        out[*at..*at + 4].copy_from_slice(&v.to_be_bytes());
        *at += 4;
    } else {
        out[*at..*at + 8].copy_from_slice(&v.to_be_bytes());
        *at += 8;
    }
    Ok(())
}

fn write_memory(w: &mut Writer<'_>, regions: &[MemRegion], addr: u32, size: u32) -> Result<(), RewriteError> {
    // 이름은 memory@<가장 낮은 시작 주소의 16진수>
    let lowest = regions.iter().map(|r| r.base).min().ok_or(RewriteError::BadRegionCount)?;
    let mut name = [0u8; 7 + 16];
    name[..7].copy_from_slice(b"memory@");
    let digits = (16 - lowest.leading_zeros() as usize / 4).max(1);
    for i in 0..digits {
        let nibble = (lowest >> ((digits - 1 - i) * 4)) & 0xF;
        name[7 + i] = b"0123456789abcdef"[nibble as usize];
    }

    let mut reg = [0u8; MAX_MEM_REGIONS * 16];
    let mut at = 0;
    for r in regions {
        push_cells(&mut reg, &mut at, addr, r.base)?;
        push_cells(&mut reg, &mut at, size, r.size)?;
    }

    w.begin(&name[..7 + digits])?;
    w.prop(b"device_type", b"memory\0")?;
    w.prop(b"reg", &reg[..at])?;
    w.end()?;
    Ok(())
}

/// 펌웨어의 DTB를 메모리 기술만 바꿔 다시 쓰는 함수입니다.
///
/// # Arguments
/// `input` - 펌웨어가 준 DTB 전체
/// `edits` - 새 메모리 구간과 더할 엔트로피
/// `out` - 출력 버퍼, 호출 때마다 전부 지우고 씀
///
/// # Errors
/// 입력의 구조 위반, 둘 이상의 `chosen`, 셀 폭에 안 들어가는 구간, 버퍼 부족은
/// `RewriteError`
pub fn rewrite(input: &[u8], edits: &Edits<'_>, out: &mut [u8]) -> Result<usize, RewriteError> {
    if edits.memory.is_empty() || edits.memory.len() > MAX_MEM_REGIONS {
        return Err(RewriteError::BadRegionCount);
    }
    let fdt = Fdt::new(input)?;
    let (addr_cells, size_cells) = root_cells(&fdt)?;
    let boot_cpuid = k0_boot::be32(input, 28)?;

    // 1차 순회: 문자열 블록에 필요한 자리의 상한(중복을 빼지 않은 합)
    let mut strings_cap = "device_type\0reg\0rng-seed\0".len();
    let mut cur = fdt.cursor();
    while let Some(tok) = cur.next()? {
        if let Token::Prop { name, .. } = tok {
            strings_cap = strings_cap.saturating_add(name.len() + 1);
        }
    }

    let mut w = Writer::new(out, &[], strings_cap)?;
    let seed = &edits.seed[..edits.seed.len().min(LOADER_SEED_MAX)];
    let mut fw_seed = [0u8; FIRMWARE_SEED_MAX];
    let mut fw_seed_len = 0;
    let mut skip: Option<usize> = None;
    let mut chosen_seen = false;
    let mut in_chosen = false;
    let mut seed_written = false;

    let write_seed = |w: &mut Writer<'_>, fw: &[u8]| -> Result<(), RewriteError> {
        let mut value = [0u8; FIRMWARE_SEED_MAX + LOADER_SEED_MAX];
        value[..fw.len()].copy_from_slice(fw);
        value[fw.len()..fw.len() + seed.len()].copy_from_slice(seed);
        let len = fw.len() + seed.len();
        let r = if len > 0 { w.prop(b"rng-seed", &value[..len]) } else { Ok(()) };
        wipe(&mut value);
        Ok(r?)
    };

    let mut cur = fdt.cursor();
    loop {
        let before = cur.depth();
        let Some(tok) = cur.next()? else { break };
        match tok {
            Token::Begin { name } => {
                if skip.is_some() {
                    continue;
                }
                let depth = before + 1;
                if depth == 2 {
                    if is_memory_name(name) || name == b"reserved-memory" {
                        skip = Some(depth);
                        continue;
                    }
                    if name == b"chosen" {
                        if chosen_seen {
                            return Err(RewriteError::DuplicateChosen);
                        }
                        chosen_seen = true;
                        in_chosen = true;
                    }
                }
                // 프로퍼티는 자식보다 앞에 와야 하므로 chosen의 첫 자식 앞에 씀
                if depth == 3 && in_chosen && !seed_written {
                    write_seed(&mut w, &fw_seed[..fw_seed_len])?;
                    seed_written = true;
                }
                w.begin(name)?;
            }
            Token::Prop { name, value } => {
                if skip.is_some() {
                    continue;
                }
                if before == 2 && in_chosen {
                    match name {
                        b"rng-seed" => {
                            fw_seed_len = value.len().min(FIRMWARE_SEED_MAX);
                            fw_seed[..fw_seed_len].copy_from_slice(&value[..fw_seed_len]);
                            continue;
                        }
                        b"linux,initrd-start" | b"linux,initrd-end" | b"linux,usable-memory-range" => continue,
                        _ => {}
                    }
                }
                w.prop(name, value)?;
            }
            Token::End => {
                if let Some(d) = skip {
                    if before == d {
                        skip = None;
                    }
                    continue;
                }
                if before == 2 && in_chosen {
                    if !seed_written {
                        write_seed(&mut w, &fw_seed[..fw_seed_len])?;
                        seed_written = true;
                    }
                    in_chosen = false;
                }
                if before == 1 {
                    // 루트를 닫기 직전. chosen이 없었는데 엔트로피가 있으면 만듦
                    if !chosen_seen && !seed.is_empty() {
                        w.begin(b"chosen")?;
                        write_seed(&mut w, &[])?;
                        w.end()?;
                    }
                    write_memory(&mut w, edits.memory, addr_cells, size_cells)?;
                }
                w.end()?;
            }
        }
    }
    wipe(&mut fw_seed);
    Ok(w.finish(boot_cpuid)?)
}
