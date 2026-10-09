use super::MemRegion;

pub const BEGIN_NODE: u32 = 0x1;
pub const END_NODE: u32 = 0x2;
pub const PROP: u32 = 0x3;
pub const NOP: u32 = 0x4;
pub const END: u32 = 0x9;

pub const OFF_TOTALSIZE: usize = 4;
pub const OFF_DT_STRUCT: usize = 8;
pub const OFF_DT_STRINGS: usize = 12;
pub const OFF_MEM_RSVMAP: usize = 16;
pub const OFF_VERSION: usize = 20;
pub const OFF_LAST_COMP: usize = 24;
pub const OFF_SIZE_STRINGS: usize = 32;
pub const OFF_SIZE_STRUCT: usize = 36;

pub struct Builder {
    rsvmap: Vec<(u64, u64)>,
    structure: Vec<u8>,
    strings: Vec<u8>,
    pub version: u32,
    pub last_comp_version: u32,
    pub terminate: bool,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            rsvmap: Vec::new(),
            structure: Vec::new(),
            strings: Vec::new(),
            version: 17,
            last_comp_version: 16,
            terminate: true,
        }
    }

    pub fn memreserve(&mut self, base: u64, size: u64) -> &mut Self {
        self.rsvmap.push((base, size));
        self
    }

    pub fn token(&mut self, t: u32) -> &mut Self {
        self.structure.extend_from_slice(&t.to_be_bytes());
        self
    }

    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.structure.extend_from_slice(bytes);
        self
    }

    fn pad(&mut self) {
        while self.structure.len() % 4 != 0 {
            self.structure.push(0);
        }
    }

    pub fn begin(&mut self, name: &str) -> &mut Self {
        self.token(BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad();
        self
    }

    pub fn end(&mut self) -> &mut Self {
        self.token(END_NODE)
    }

    pub fn nop(&mut self) -> &mut Self {
        self.token(NOP)
    }

    pub fn string_offset(&mut self, name: &str) -> u32 {
        let mut off = 0;
        for s in self.strings.split(|&b| b == 0) {
            if s == name.as_bytes() && off < self.strings.len() {
                return off as u32;
            }
            off += s.len() + 1;
        }
        let off = self.strings.len() as u32;
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        off
    }

    pub fn prop_at(&mut self, name_off: u32, value: &[u8]) -> &mut Self {
        self.token(PROP);
        self.structure.extend_from_slice(&(value.len() as u32).to_be_bytes());
        self.structure.extend_from_slice(&name_off.to_be_bytes());
        self.structure.extend_from_slice(value);
        self.pad();
        self
    }

    pub fn prop(&mut self, name: &str, value: &[u8]) -> &mut Self {
        let off = self.string_offset(name);
        self.prop_at(off, value)
    }

    pub fn prop_u32(&mut self, name: &str, v: u32) -> &mut Self {
        self.prop(name, &v.to_be_bytes())
    }

    pub fn build(&self) -> Vec<u8> {
        let mut rsv = Vec::new();
        for &(b, s) in &self.rsvmap {
            rsv.extend_from_slice(&b.to_be_bytes());
            rsv.extend_from_slice(&s.to_be_bytes());
        }
        rsv.extend_from_slice(&[0u8; 16]);

        let mut st = self.structure.clone();
        if self.terminate {
            st.extend_from_slice(&END.to_be_bytes());
        }

        let off_rsv = 40usize;
        let off_struct = off_rsv + rsv.len();
        let off_strings = off_struct + st.len();
        let total = off_strings + self.strings.len();

        let mut out = Vec::with_capacity(total);
        for v in [
            0xd00d_feed,
            total as u32,
            off_struct as u32,
            off_strings as u32,
            off_rsv as u32,
            self.version,
            self.last_comp_version,
            0,
            self.strings.len() as u32,
            st.len() as u32,
        ] {
            out.extend_from_slice(&u32::to_be_bytes(v));
        }
        out.extend_from_slice(&rsv);
        out.extend_from_slice(&st);
        out.extend_from_slice(&self.strings);
        out
    }
}

pub fn set_be32(blob: &mut [u8], off: usize, v: u32) {
    blob[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

pub fn get_be32(blob: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(blob[off..off + 4].try_into().unwrap())
}

pub fn reg(entries: &[(u64, u64)], addr_cells: u32, size_cells: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for &(b, s) in entries {
        push_cells(&mut v, b, addr_cells);
        push_cells(&mut v, s, size_cells);
    }
    v
}

fn push_cells(v: &mut Vec<u8>, x: u64, cells: u32) {
    for i in (0..cells).rev() {
        let word = if i >= 2 { 0 } else { (x >> (32 * i)) as u32 };
        v.extend_from_slice(&word.to_be_bytes());
    }
}

#[derive(Clone, Debug)]
pub struct Board {
    pub cells: Option<(u32, u32)>,
    pub memory: Vec<Vec<(u64, u64)>>,
    pub memreserve: Vec<(u64, u64)>,
    pub rsv_cells: Option<(u32, u32)>,
    pub rsv_children: Vec<Option<Vec<(u64, u64)>>>,
    pub rng_seed: Option<Vec<u8>>,
    pub kaslr_seed: Option<Vec<u8>>,
}

impl Board {
    pub fn empty() -> Self {
        Self {
            cells: None,
            memory: Vec::new(),
            memreserve: Vec::new(),
            rsv_cells: None,
            rsv_children: Vec::new(),
            rng_seed: None,
            kaslr_seed: None,
        }
    }

    pub fn qemu_virt() -> Self {
        Self {
            cells: Some((2, 2)),
            memory: vec![vec![(0x4000_0000, 0x2000_0000)]],
            rng_seed: Some((0u8..32).collect()),
            kaslr_seed: Some(vec![0xA5; 8]),
            ..Self::empty()
        }
    }

    fn root_cells(&self) -> (u32, u32) {
        self.cells.unwrap_or((2, 1))
    }

    pub fn write(&self, b: &mut Builder, noise: &mut dyn FnMut(&mut Builder)) {
        for &(base, size) in &self.memreserve {
            b.memreserve(base, size);
        }
        let (ac, sc) = self.root_cells();
        b.begin("");
        if let Some((a, s)) = self.cells {
            b.prop_u32("#address-cells", a).prop_u32("#size-cells", s);
        }
        b.prop("compatible", b"linux,dummy-virt\0");
        noise(b);
        if self.rng_seed.is_some() || self.kaslr_seed.is_some() {
            b.begin("chosen");
            noise(b);
            if let Some(seed) = &self.rng_seed {
                b.prop("rng-seed", seed);
            }
            b.prop("bootargs", b"console=ttyAMA0\0");
            if let Some(seed) = &self.kaslr_seed {
                b.prop("kaslr-seed", seed);
            }
            noise(b);
            b.end();
        }
        for (i, entries) in self.memory.iter().enumerate() {
            let name = match entries.first() {
                Some(&(base, _)) if i % 2 == 0 => format!("memory@{base:x}"),
                _ => "memory".to_string(),
            };
            b.begin(&name);
            b.prop("device_type", b"memory\0");
            noise(b);
            b.prop("reg", &reg(entries, ac, sc));
            noise(b);
            b.end();
            noise(b);
        }
        if !self.rsv_children.is_empty() || self.rsv_cells.is_some() {
            let (rac, rsc) = self.rsv_cells.unwrap_or((ac, sc));
            b.begin("reserved-memory");
            if let Some((a, s)) = self.rsv_cells {
                b.prop_u32("#address-cells", a).prop_u32("#size-cells", s);
            }
            b.prop("ranges", b"");
            noise(b);
            for (i, child) in self.rsv_children.iter().enumerate() {
                b.begin(&format!("region@{i}"));
                match child {
                    Some(entries) => {
                        b.prop("reg", &reg(entries, rac, rsc));
                    }
                    None => {
                        b.prop("size", &reg(&[(0, 0x10_0000)], 0, rsc));
                    }
                }
                b.prop("no-map", b"");
                noise(b);
                b.end();
            }
            b.end();
        }
        noise(b);
        b.end();
    }

    pub fn blob(&self) -> Vec<u8> {
        let mut b = Builder::new();
        self.write(&mut b, &mut |_| {});
        b.build()
    }

    pub fn expected_memory(&self) -> Vec<MemRegion> {
        self.memory
            .iter()
            .flatten()
            .filter(|&&(_, s)| s != 0)
            .map(|&(base, size)| MemRegion { base, size })
            .collect()
    }

    pub fn expected_reserved(&self) -> Vec<MemRegion> {
        let rsv = self.memreserve.iter().copied().take_while(|&e| e != (0, 0));
        let children = self.rsv_children.iter().flatten().flatten().copied();
        rsv.chain(children)
            .filter(|&(_, s)| s != 0)
            .map(|(base, size)| MemRegion { base, size })
            .collect()
    }

    pub fn expected_entropy(&self) -> Vec<u8> {
        let mut e: Vec<u8> = self.rng_seed.iter().chain(self.kaslr_seed.iter()).flatten().copied().collect();
        e.truncate(super::MAX_ENTROPY);
        e
    }
}

#[test]
fn builder_emits_a_well_formed_header() {
    let blob = Board::qemu_virt().blob();
    assert_eq!(get_be32(&blob, 0), 0xd00d_feed);
    assert_eq!(get_be32(&blob, OFF_TOTALSIZE) as usize, blob.len());
    assert_eq!(get_be32(&blob, OFF_MEM_RSVMAP), 40);
    assert_eq!(get_be32(&blob, OFF_VERSION), 17);
    assert_eq!(get_be32(&blob, OFF_LAST_COMP), 16);
    let off_struct = get_be32(&blob, OFF_DT_STRUCT) as usize;
    let size_struct = get_be32(&blob, OFF_SIZE_STRUCT) as usize;
    assert_eq!(off_struct % 4, 0);
    assert_eq!(size_struct % 4, 0);
    assert_eq!(get_be32(&blob, off_struct), BEGIN_NODE);
    assert_eq!(get_be32(&blob, off_struct + size_struct - 4), END);
    assert_eq!(
        get_be32(&blob, OFF_DT_STRINGS) as usize + get_be32(&blob, OFF_SIZE_STRINGS) as usize,
        blob.len()
    );
}

#[test]
fn builder_deduplicates_property_names() {
    let mut b = Builder::new();
    let a = b.string_offset("reg");
    let c = b.string_offset("compatible");
    assert_eq!(b.string_offset("reg"), a);
    assert_eq!(b.string_offset("compatible"), c);
    assert_ne!(a, c);
}

#[test]
fn reg_encodes_big_endian_cells() {
    assert_eq!(reg(&[(0x1_2345_6789, 0x10)], 2, 1), [0, 0, 0, 1, 0x23, 0x45, 0x67, 0x89, 0, 0, 0, 0x10]);
    assert_eq!(reg(&[(0x8000_0000, 0x1000)], 1, 1), [0x80, 0, 0, 0, 0, 0, 0x10, 0]);
}
