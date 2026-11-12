//! 수신 로직을 가짜 UART로 검증하는 테스트 모듈입니다.
//!
//! 가짜 UART는 두 가지로 돕니다. 하나는 QEMU의 PL011처럼 FIFO가 차면 바이트를
//! 받지 않고 기다리게 하는 것(넘침 없음, 수신 타임아웃 없음)이고, 다른 하나는
//! 실제 하드웨어처럼 FIFO가 차면 바이트를 잃고 넘침을 알리는 것입니다.

use std::collections::VecDeque;

use super::*;

const BREAK: u32 = DR_BE | DR_FE;

struct Fake {
    fifo: VecDeque<u32>,
    depth: usize,
    /// FIFO가 이만큼 차는 순간 수신 인터럽트가 섬
    trigger: usize,
    /// FIFO가 차면 바이트를 거절하는지(QEMU), 아니면 잃는지(하드웨어)
    backpressure: bool,
    has_cr: bool,
    ris: u32,
    imsc: u32,
    cr: u32,
    /// 넘친 뒤 다음 데이터 읽기에 넘침 비트를 실을지
    overrun: bool,
    accesses: usize,
    dr_reads: usize,
    cr_writes: usize,
}

impl Fake {
    fn new(depth: usize, trigger: usize, backpressure: bool) -> Self {
        Self {
            fifo: VecDeque::new(),
            depth,
            trigger,
            backpressure,
            has_cr: true,
            ris: 0,
            imsc: 0,
            cr: 0x300,
            overrun: false,
            accesses: 0,
            dr_reads: 0,
            cr_writes: 0,
        }
    }

    fn line(&self) -> bool {
        self.ris & self.imsc != 0
    }

    /// 선로에서 문자 하나가 도착함. 받았으면 true, 거절했거나 잃었으면 false
    fn offer(&mut self, entry: u32) -> bool {
        if self.fifo.len() == self.depth {
            if !self.backpressure {
                self.ris |= INT_OE;
                self.overrun = true;
            }
            return false;
        }
        self.fifo.push_back(entry);
        if self.fifo.len() == self.trigger {
            self.ris |= INT_RX;
        }
        true
    }

    /// 선로가 한동안 조용했음(수신 타임아웃)
    fn idle(&mut self) {
        if !self.fifo.is_empty() && !self.backpressure {
            self.ris |= INT_RT;
        }
    }
}

impl Regs for Fake {
    fn read(&mut self, reg: Reg) -> u32 {
        self.accesses += 1;
        match reg {
            Reg::Dr => {
                self.dr_reads += 1;
                let mut d = self.fifo.pop_front().unwrap_or(0);
                if self.overrun {
                    d |= DR_OE;
                    self.overrun = false;
                }
                if self.fifo.len() < self.trigger {
                    self.ris &= !INT_RX;
                }
                if self.fifo.is_empty() {
                    self.ris &= !INT_RT;
                }
                d
            }
            Reg::Fr => {
                if self.fifo.is_empty() {
                    FR_RXFE
                } else {
                    0
                }
            }
            Reg::Cr => {
                assert!(self.has_cr, "the SBSA UART has no control register");
                self.cr
            }
            Reg::Imsc => self.imsc,
            Reg::Mis => self.ris & self.imsc,
            Reg::Icr => 0,
        }
    }

    fn write(&mut self, reg: Reg, value: u32) {
        self.accesses += 1;
        match reg {
            Reg::Imsc => self.imsc = value,
            Reg::Icr => self.ris &= !value,
            Reg::Cr => {
                assert!(self.has_cr, "the SBSA UART has no control register");
                self.cr = value;
                self.cr_writes += 1;
            }
            other => panic!("write to {other:?}"),
        }
    }
}

/// 고정된 씨앗의 의사 난수입니다(xorshift64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// UART, 수신 로직, 인터럽트 컨트롤러의 선 하나, 읽는 태스크를 묶은 모형입니다.
struct Sim {
    uart: Fake,
    rx: Rx,
    /// 인터럽트 컨트롤러에서 이 선이 켜져 있는지
    enabled: bool,
    /// 선로에 내보냈지만 UART가 아직 받지 않은 것(QEMU 모형에서만 쌓임)
    waiting: VecDeque<u32>,
    /// 보낸 순서 번호를 붙인, UART가 받은 바이트
    accepted: Vec<(usize, u8)>,
    sent: usize,
    got: Vec<(u8, bool)>,
    entries: usize,
}

impl Sim {
    fn new(uart: Fake) -> Self {
        let mut s = Self {
            rx: Rx::new(uart.has_cr),
            uart,
            enabled: true,
            waiting: VecDeque::new(),
            accepted: Vec::new(),
            sent: 0,
            got: Vec::new(),
            entries: 0,
        };
        Rx::quiesce(&mut s.uart);
        s.rx.open(&mut s.uart);
        s
    }

    /// 선이 서 있는 동안 핸들러를 부름. 핸들러가 돌아온 뒤에도 선이 서 있으면
    /// 곧바로 다시 들어오는 것이고, 그런 재진입은 끊기 전까지의 횟수로 묶여야 함
    fn irqs(&mut self) {
        let mut again = 0;
        while self.enabled && self.uart.line() {
            self.entries += 1;
            again += 1;
            assert!(again <= SPURIOUS_MAX as usize, "the handler returned with the line still up, again and again");
            if self.rx.interrupt(&mut self.uart) == Irq::CutOff {
                self.enabled = false;
            }
        }
    }

    /// 기다리던 문자를 UART에 밀어 넣고 인터럽트를 돌림
    fn pump(&mut self) {
        loop {
            self.irqs();
            let Some(&e) = self.waiting.front() else { break };
            let index = self.sent;
            if self.uart.offer(e) {
                self.waiting.pop_front();
                self.sent += 1;
                if e & BREAK == 0 {
                    self.accepted.push((index, e as u8));
                }
            } else if self.uart.backpressure {
                self.irqs();
                break;
            } else {
                // 하드웨어 모형: 받지 못한 문자는 사라짐
                self.waiting.pop_front();
                self.sent += 1;
            }
        }
        self.irqs();
    }

    fn send(&mut self, bytes: &[u8]) {
        self.waiting.extend(bytes.iter().map(|b| u32::from(*b)));
        self.pump();
    }

    fn pop(&mut self) -> bool {
        let r = self.rx.pop(&mut self.uart);
        if let Some(e) = r {
            self.got.push(e);
        }
        self.pump();
        r.is_some()
    }

    fn tick(&mut self) {
        self.rx.poll(&mut self.uart);
        self.pump();
    }

    /// 더 올 것이 없을 때까지 읽는 쪽을 돌림(읽는 태스크는 잠들기 전에 한 번
    /// 들여다보고, 틱마다 커널이 한 번 더 들여다봄)
    fn settle(&mut self) {
        for _ in 0..10_000 {
            self.uart.idle();
            self.pump();
            if self.pop() {
                continue;
            }
            self.tick();
            if !self.rx.has_data() && self.waiting.is_empty() && self.uart.fifo.is_empty() {
                return;
            }
        }
        panic!("input never drained: {} waiting, {} in the fifo", self.waiting.len(), self.uart.fifo.len());
    }

    /// 받은 것이 UART가 받아들인 것과 순서까지 같고, 빠진 자리 뒤의 첫
    /// 바이트에는 유실 표시가 있어야 함
    fn check(&self) {
        let bytes: Vec<u8> = self.got.iter().map(|e| e.0).collect();
        let want: Vec<u8> = self.accepted.iter().map(|e| e.1).collect();
        assert_eq!(bytes, want);
        let mut last = None;
        for (k, (index, _)) in self.accepted.iter().enumerate() {
            let hole = match last {
                None => *index != 0,
                Some(prev) => *index != prev + 1,
            };
            if hole {
                assert!(self.got[k].1, "byte {k} follows a hole and carries no mark");
            }
            last = Some(*index);
        }
    }
}

#[test]
fn quiesce_masks_everything_and_open_asks_only_for_receive() {
    let mut uart = Fake::new(16, 1, true);
    uart.imsc = 0x7FF;
    uart.ris = 1 << 5;
    Rx::quiesce(&mut uart);
    assert_eq!((uart.imsc, uart.ris), (0, 0));
    assert!(!uart.line());

    // 열기 전에 온 바이트는 버려지고, UART와 수신기가 켜지며, 수신 관련
    // 인터럽트만 요청함
    for b in b"typed during boot" {
        uart.offer(u32::from(*b));
    }
    let mut rx = Rx::new(true);
    assert!(!rx.is_open());
    rx.open(&mut uart);
    assert!(rx.is_open() && !rx.has_data() && uart.fifo.is_empty());
    assert_eq!(uart.cr, 0x301);
    assert_eq!(uart.imsc, INT_RX | INT_RT | INT_OE);
    assert_eq!(rx.pop(&mut uart), None);

    // 이미 켜져 있으면 제어 레지스터에 쓰지 않고, 두 번 여는 것은 아무 일도 아님
    let mut uart = Fake::new(16, 1, true);
    uart.cr = 0x301;
    let mut rx = Rx::new(true);
    rx.open(&mut uart);
    let after = uart.accesses;
    rx.open(&mut uart);
    assert_eq!((uart.cr_writes, uart.accesses), (0, after));
}

#[test]
fn the_sbsa_uart_is_never_asked_for_a_control_register() {
    let mut uart = Fake::new(32, 16, false);
    uart.has_cr = false;
    let mut sim = Sim::new(uart);
    sim.send(b"hello over an sbsa uart, longer than the fifo trigger");
    sim.settle();
    sim.check();
    assert_eq!(sim.got.len(), sim.accepted.len());
}

#[test]
fn a_byte_arrives_through_one_interrupt() {
    let mut sim = Sim::new(Fake::new(1, 1, true));
    sim.send(b"a");
    assert_eq!(sim.entries, 1);
    assert!(!sim.uart.line());
    assert_eq!(sim.rx.pop(&mut sim.uart), Some((b'a', false)));
    assert_eq!(sim.rx.pop(&mut sim.uart), None);
}

#[test]
fn a_flood_fills_the_ring_closes_the_gate_and_resumes_when_the_reader_catches_up() {
    // 설계 검토가 찾은 경우: 링이 가득 찬 뒤 게이트가 열릴 때 FIFO에 남은
    // 바이트를 가져올 인터럽트가 다시 오지 않으면 입력이 영영 멈춤
    for (depth, trigger) in [(1, 1), (16, 1), (32, 16)] {
        let mut sim = Sim::new(Fake::new(depth, trigger, true));
        let flood: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        sim.send(&flood);
        // 읽는 쪽이 멈춰 있으면 링이 차고 선이 내려감. 그때까지의 진입 횟수는
        // 링 크기를 넘지 않음
        assert!(sim.rx.has_data());
        assert!(!sim.uart.line());
        assert!(sim.entries <= RING + 1, "{} entries", sim.entries);
        assert_eq!(sim.uart.imsc, 0, "the gate is closed at the uart");
        let quiet = sim.entries;
        for _ in 0..50 {
            sim.pump();
        }
        assert_eq!(sim.entries, quiet, "no interrupt arrives while the reader is stopped");
        sim.settle();
        sim.check();
        assert_eq!(sim.got.len(), 5000);
        assert!(sim.got.iter().all(|e| !e.1), "nothing was lost, so nothing is marked");
        assert_eq!(sim.uart.imsc, INT_RX | INT_RT | INT_OE, "the gate is open again");
    }
}

#[test]
fn the_reader_alone_reopens_the_gate_without_waiting_for_a_tick() {
    for (depth, trigger, backpressure) in [(1, 1, true), (16, 1, true), (32, 16, false)] {
        let mut sim = Sim::new(Fake::new(depth, trigger, backpressure));
        sim.send(&[b'a'; RING + 40]);
        assert_eq!(sim.uart.imsc, 0);
        // 틱도 잠들기 전 확인도 없이 꺼내기만 함
        let mut popped = 0;
        while sim.pop() {
            popped += 1;
            assert!(popped < 10_000);
        }
        if backpressure {
            assert_eq!(popped, RING + 40);
        } else {
            assert!(popped >= RING + depth);
        }
        assert_eq!(sim.uart.imsc, INT_RX | INT_RT | INT_OE);
        assert!(!sim.uart.line());
        // 그 뒤의 바이트는 다시 인터럽트로 들어옴
        let before = sim.entries;
        sim.send(&[b'b'; 32]);
        assert!(sim.entries > before && sim.rx.has_data());
    }
}

#[test]
fn the_work_of_one_interrupt_is_bounded_by_the_room_in_the_ring() {
    let mut sim = Sim::new(Fake::new(32, 16, false));
    for round in 0..40 {
        let before = sim.uart.dr_reads;
        let room = RING - sim.rx.len;
        sim.send(&[round as u8; 16]);
        assert!(sim.uart.dr_reads - before <= room.max(1) + 16);
    }
}

#[test]
fn random_schedules_on_the_emulator_model_deliver_every_byte_exactly_once() {
    let mut rng = Rng(0x243F_6A88_85A3_08D3);
    for round in 0..300 {
        let depth = [1usize, 16, 32][rng.below(3) as usize];
        let mut sim = Sim::new(Fake::new(depth, 1, true));
        for _ in 0..rng.below(60) {
            match rng.below(10) {
                0..=3 => {
                    let n = [1, 3, 40, 300, 700][rng.below(5) as usize];
                    let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                    sim.send(&bytes);
                }
                4..=7 => {
                    for _ in 0..rng.below(200) {
                        sim.pop();
                    }
                }
                _ => sim.tick(),
            }
        }
        sim.settle();
        sim.check();
        assert_eq!(sim.got.len(), sim.sent, "round {round}");
        assert!(sim.got.iter().all(|e| !e.1), "round {round}: a mark without a loss");
        assert!(sim.enabled, "round {round}: a healthy uart was cut off");
    }
}

#[test]
fn random_schedules_on_the_hardware_model_mark_every_hole() {
    let mut rng = Rng(0x1319_8A2E_0370_7344);
    let mut holes = 0;
    for round in 0..400 {
        let (depth, trigger) = [(16usize, 8usize), (32, 16), (32, 4), (1, 1)][rng.below(4) as usize];
        let mut sim = Sim::new(Fake::new(depth, trigger, false));
        for _ in 0..rng.below(60) {
            match rng.below(12) {
                0..=3 => {
                    let n = [1, 2, 7, 20, 90, 400][rng.below(6) as usize];
                    let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                    sim.send(&bytes);
                }
                4 => {
                    // 선로의 break는 오류 표시가 붙은 문자로 들어옴
                    sim.waiting.push_back(BREAK);
                    sim.pump();
                }
                5..=8 => {
                    for _ in 0..rng.below(300) {
                        sim.pop();
                    }
                }
                9 => {
                    sim.uart.idle();
                    sim.pump();
                }
                _ => sim.tick(),
            }
        }
        sim.settle();
        sim.check();
        holes += usize::from(sim.accepted.len() != sim.sent);
        if sim.accepted.len() == sim.sent {
            assert!(sim.got.iter().all(|e| !e.1), "round {round}: a mark without a loss");
        }
        assert!(sim.enabled, "round {round}: a healthy uart was cut off");
    }
    assert!(holes > 100, "the schedules must actually overrun the fifo ({holes} rounds did)");
}

#[test]
fn a_break_marks_the_byte_after_it() {
    let mut sim = Sim::new(Fake::new(16, 1, true));
    sim.send(b"vault zeroize con");
    sim.waiting.push_back(BREAK);
    sim.pump();
    sim.send(b"firm\r");
    sim.settle();
    let marks: Vec<usize> = sim.got.iter().enumerate().filter(|(_, e)| e.1).map(|(i, _)| i).collect();
    assert_eq!(marks, [17]);
    sim.check();
}

#[test]
fn an_overrun_marks_what_was_in_the_fifo_and_the_first_byte_after() {
    // 읽는 쪽이 멈춘 동안 링과 FIFO가 모두 차서 바이트가 사라짐
    let mut sim = Sim::new(Fake::new(16, 8, false));
    let first: Vec<u8> = (0..RING as u32 + 16).map(|i| i as u8).collect();
    sim.send(&first);
    sim.send(b"these bytes are lost");
    assert_eq!(sim.accepted.len(), RING + 16);
    // 자리가 난 뒤에 온 바이트는 받음
    for _ in 0..200 {
        sim.pop();
    }
    sim.send(b"after");
    sim.settle();
    sim.check();
    // 구멍은 FIFO에 있던 16바이트의 뒤이지만 경계를 알 수 없으므로 그 16바이트와
    // 뒤의 첫 바이트가 모두 표시됨. 링에 먼저 들어가 있던 바이트는 표시되지 않음
    let marks: Vec<usize> = sim.got.iter().enumerate().filter(|(_, e)| e.1).map(|(i, _)| i).collect();
    assert_eq!(marks.first(), Some(&RING));
    assert_eq!(marks.last(), Some(&(RING + 16)));
    assert!(sim.got[..RING].iter().all(|e| !e.1));
    assert!(!sim.got[RING + 17].1);
}

#[test]
fn flush_drops_the_ring_the_fifo_and_the_marks() {
    let mut sim = Sim::new(Fake::new(16, 8, false));
    sim.send(&[b'x'; 400]);
    sim.send(b"lost");
    assert!(sim.rx.has_data());
    sim.rx.flush(&mut sim.uart);
    assert!(!sim.rx.has_data() && sim.uart.fifo.is_empty());
    assert_eq!(sim.uart.imsc, INT_RX | INT_RT | INT_OE);
    assert_eq!(sim.uart.ris & INT_OE, 0);
    sim.got.clear();
    sim.send(b"help\r");
    sim.settle();
    assert_eq!(sim.got, [(b'h', false), (b'e', false), (b'l', false), (b'p', false), (b'\r', false)]);

    // 닫혀 있는 경로에서는 아무 일도 하지 않음
    let mut uart = Fake::new(16, 8, false);
    let mut rx = Rx::new(true);
    rx.flush(&mut uart);
    assert_eq!(uart.accesses, 0);
}

#[test]
fn input_still_arrives_when_the_interrupt_never_comes() {
    // 디바이스 트리가 조용한 다른 선을 가리킨 경우, 또는 수신 타임아웃이 없는
    // UART에서 FIFO가 트리거에 못 미친 경우: 틱마다 들여다보는 것으로 도착함
    let mut sim = Sim::new(Fake::new(32, 16, false));
    sim.enabled = false;
    sim.send(b"help\r");
    assert!(!sim.rx.has_data());
    sim.tick();
    assert!(sim.rx.has_data());
    sim.settle();
    sim.check();
    assert_eq!(sim.got.len(), 5);
}

/// 원인을 밝히지 않으면서 선을 붙들고 있는 장치입니다(다른 장치의 선).
struct Stuck {
    accesses: usize,
    mis: u32,
}

impl Regs for Stuck {
    fn read(&mut self, reg: Reg) -> u32 {
        self.accesses += 1;
        match reg {
            Reg::Mis => self.mis,
            Reg::Fr => FR_RXFE,
            _ => 0,
        }
    }

    fn write(&mut self, _reg: Reg, _value: u32) {
        self.accesses += 1;
    }
}

#[test]
fn a_line_that_stays_up_without_a_cause_is_cut_off_after_a_few_entries() {
    // 원인이 없다고 하는 경우와, 수신이라고 하면서 FIFO가 비어 있는 경우
    for mis in [0, INT_RX, INT_RT] {
        let mut regs = Stuck { accesses: 0, mis };
        let mut rx = Rx::new(true);
        rx.open(&mut regs);
        for _ in 0..SPURIOUS_MAX - 1 {
            assert_eq!(rx.interrupt(&mut regs), Irq::Spurious);
            assert!(!rx.is_off());
        }
        assert_eq!(rx.interrupt(&mut regs), Irq::CutOff);
        assert!(rx.is_off() && !rx.is_open() && !rx.has_data());
        // 끈 뒤에 들어오는 인터럽트는 레지스터를 건드리지 않음
        let after = regs.accesses;
        assert_eq!(rx.interrupt(&mut regs), Irq::Spurious);
        rx.poll(&mut regs);
        rx.flush(&mut regs);
        rx.open(&mut regs);
        assert_eq!(rx.pop(&mut regs), None);
        assert_eq!(regs.accesses, after);
        assert!(rx.is_off());
    }
}

#[test]
fn a_real_cause_resets_the_spurious_count() {
    let mut sim = Sim::new(Fake::new(16, 1, true));
    for round in 0..50 {
        // 원인 없는 진입을 끊기 직전까지 넣고, 바이트 하나로 되돌림
        for _ in 0..SPURIOUS_MAX - 1 {
            assert_eq!(sim.rx.interrupt(&mut sim.uart), Irq::Spurious);
        }
        sim.send(&[round]);
        assert!(!sim.rx.is_off());
    }
    sim.settle();
    assert_eq!(sim.got.len(), 50);
}

#[test]
fn turning_the_path_off_masks_the_uart_and_empties_the_ring() {
    let mut sim = Sim::new(Fake::new(16, 1, true));
    sim.send(b"abc");
    sim.rx.turn_off(&mut sim.uart);
    assert_eq!(sim.uart.imsc, 0);
    assert!(sim.rx.is_off() && !sim.rx.has_data());
    assert_eq!(sim.rx.pop(&mut sim.uart), None);
}
