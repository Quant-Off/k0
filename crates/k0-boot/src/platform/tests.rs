use std::panic::{catch_unwind, AssertUnwindSafe};

use super::*;
use crate::fdt::builder::Builder;
use crate::testing::*;

fn run(tree: &N) -> Result<Discovery, DiscoverError> {
    discover_blob(&tree.blob())
}

fn ok(tree: &N) -> Discovery {
    run(tree).unwrap_or_else(|e| panic!("{e:?}"))
}

fn no_console(tree: &N) -> NoConsole {
    let d = ok(tree);
    assert_eq!(d.platform.console, None);
    d.no_console.expect("a reason")
}

fn redist1(base: u64, size: u64) -> [Region; MAX_REDIST_REGIONS] {
    let mut r = [Region { base: 0, size: 0 }; MAX_REDIST_REGIONS];
    r[0] = Region { base, size };
    r
}

#[test]
fn qemu_virt_gicv3_is_discovered() {
    let d = ok(&virt3());
    assert_eq!(d.no_console, None);
    assert_eq!(
        d.platform,
        Platform {
            console: Some(Console {
                kind: ConsoleKind::Pl011,
                base: 0x900_0000,
                span: 0x1C,
                reg: Region { base: 0x900_0000, size: 0x1000 },
            }),
            intc: Intc::GicV3 {
                dist: Region { base: 0x800_0000, size: 0x1_0000 },
                redist: redist1(0x80a_0000, 0xf6_0000),
                redist_count: 1,
                stride: 0,
            },
            timer: Timer { intid: 27, clock_hz: 0 },
        }
    );
    assert_eq!(
        d.platform.to_string(),
        "con=pl011@0x9000000 gic=v3@0x8000000 rd=0x80a0000+0xf60000 tmr=27"
    );
}

#[test]
fn qemu_virt_gicv2_is_discovered() {
    let d = ok(&virt2());
    assert_eq!(
        d.platform.intc,
        Intc::GicV2 {
            dist: Region { base: 0x800_0000, size: 0x1_0000 },
            cpu: Region { base: 0x801_0000, size: 0x1_0000 },
        }
    );
    assert_eq!(d.platform.timer.intid, 27);
    assert_eq!(
        d.platform.to_string(),
        "con=pl011@0x9000000 gic=v2@0x8000000 cpu=0x8010000 tmr=27"
    );
}

#[test]
fn raspberry_pi_4_shape_is_translated_through_soc_ranges() {
    let d = ok(&rpi4());
    assert_eq!(
        d.platform.console,
        Some(Console {
            kind: ConsoleKind::Ns16550 { reg_shift: 2, io_width: 4 },
            base: 0xfe21_5040,
            span: 24,
            reg: Region { base: 0xfe21_5040, size: 0x40 },
        })
    );
    assert_eq!(
        d.platform.intc,
        Intc::GicV2 {
            dist: Region { base: 0xff84_1000, size: 0x1000 },
            cpu: Region { base: 0xff84_2000, size: 0x2000 },
        }
    );
    assert_eq!(
        d.platform.to_string(),
        "con=ns16550@0xfe215040,s2,w4 gic=v2@0xff841000 cpu=0xff842000 tmr=27"
    );
}

#[test]
fn rk3399_shape_uses_four_cell_specifiers_and_a_shifted_uart() {
    let d = ok(&rk3399());
    assert_eq!(
        d.platform.console,
        Some(Console {
            kind: ConsoleKind::Ns16550 { reg_shift: 2, io_width: 4 },
            base: 0xff1a_0000,
            span: 24,
            reg: Region { base: 0xff1a_0000, size: 0x100 },
        })
    );
    assert_eq!(d.platform.intc.redist(), &[Region { base: 0xfef0_0000, size: 0xc_0000 }]);
    assert_eq!(d.platform.timer.intid, 27);
}

#[test]
fn the_virtual_timer_number_comes_from_the_tree() {
    // Qualcomm sdm845: <1 1>, <1 2>, <1 3>, <1 0>
    let mut t = virt3();
    t.kid("timer").set("interrupts", cells(&[1, 1, 8, 1, 2, 8, 1, 3, 8, 1, 0, 8]));
    assert_eq!(ok(&t).platform.timer.intid, 19);

    // interrupt-names가 있으면 이름이 순서보다 우선함
    let mut t = virt3();
    t.kid("timer").set("interrupts", cells(&[1, 14, 4, 1, 9, 4]));
    t.kid("timer").set("interrupt-names", strs(&["phys", "virt"]));
    assert_eq!(ok(&t).platform.timer.intid, 25);

    let mut t = virt3();
    t.kid("timer").set("clock-frequency", cells(&[24_000_000]));
    let d = ok(&t);
    assert_eq!(d.platform.timer.clock_hz, 24_000_000);
    assert!(d.platform.to_string().ends_with("tmr=27 hz=24000000"));
}

#[test]
fn a_missing_or_unusable_console_is_not_an_error() {
    let mut t = virt3();
    t.kid("chosen").del("stdout-path");
    assert_eq!(no_console(&t), NoConsole::NoStdoutPath);

    // 옛 이름도 받음
    t.kid("chosen").set("linux,stdout-path", strs(&["/pl011@9000000"]));
    assert!(ok(&t).platform.console.is_some());

    let cases: [(&str, Vec<u8>, NoConsole); 7] = [
        ("unterminated", b"/pl011@9000000".to_vec(), NoConsole::BadPath),
        ("empty", strs(&[""]), NoConsole::BadPath),
        ("options only", strs(&[":115200"]), NoConsole::BadPath),
        ("double slash", strs(&["//pl011@9000000"]), NoConsole::BadPath),
        ("unknown alias", strs(&["serial0"]), NoConsole::UnknownAlias),
        ("no such node", strs(&["/pl011@9000001"]), NoConsole::NotFound),
        ("name without unit address", strs(&["/pl011"]), NoConsole::NotFound),
    ];
    for (what, value, want) in cases {
        let mut t = virt3();
        t.kid("chosen").set("stdout-path", value);
        assert_eq!(no_console(&t), want, "{what}");
    }

    // 옵션은 ':' 뒤에서 잘림
    let mut t = virt3();
    t.kid("chosen").set("stdout-path", strs(&["/pl011@9000000:115200n8"]));
    assert!(ok(&t).platform.console.is_some());
}

#[test]
fn aliases_resolve_once_and_only_to_full_paths() {
    let mut t = virt3();
    t.kid("chosen").set("stdout-path", strs(&["serial0"]));
    let with = |v: Vec<u8>| {
        let mut t = t.clone();
        t.kids.insert(0, N::new("aliases").p("serial0", v).p("other", strs(&["/pl011@9000000"])));
        t
    };
    assert!(ok(&with(strs(&["/pl011@9000000"]))).platform.console.is_some());
    assert_eq!(no_console(&with(strs(&["other"]))), NoConsole::BadPath);
    assert_eq!(no_console(&with(b"/pl011@9000000".to_vec())), NoConsole::BadPath);

    // 더 깊은 곳의 aliases 노드는 별칭 표가 아님
    let mut nested = t.clone();
    nested
        .kids
        .insert(0, N::new("soc").c(N::new("aliases").p("serial0", strs(&["/pl011@9000000"]))));
    assert_eq!(no_console(&nested), NoConsole::UnknownAlias);
}

#[test]
fn console_nodes_are_checked_before_use() {
    let mut t = virt3();
    t.kid("pl011@9000000").set("status", strs(&["disabled"]));
    assert_eq!(no_console(&t), NoConsole::Disabled);

    let mut t = virt3();
    t.kid("pl011@9000000").set("status", strs(&["ok"]));
    assert!(ok(&t).platform.console.is_some());

    let mut t = rpi4();
    t.kid("soc").set("status", strs(&["reserved"]));
    // /soc가 꺼지면 그 아래 인터럽트 컨트롤러도 못 씀
    assert_eq!(run(&t), Err(DiscoverError::IntcDisabled));

    let mut t = virt3();
    t.kid("pl011@9000000").set("compatible", strs(&["cdns,uart-r1p12", "xlnx,xuartps"]));
    assert_eq!(no_console(&t), NoConsole::UnsupportedUart);

    let mut t = virt3();
    t.kid("pl011@9000000").set("big-endian", flag());
    assert_eq!(no_console(&t), NoConsole::UnsupportedUart);

    let mut t = virt3();
    t.kid("pl011@9000000").set("reg", cells(&[0, 0x900_0000, 0, 0x18]));
    assert_eq!(no_console(&t), NoConsole::BadProperties);

    // 같은 이름의 형제가 둘이면 어느 쪽인지 고르지 않음
    let mut t = virt3();
    t.kids.push(pl011());
    assert_eq!(no_console(&t), NoConsole::Ambiguous);

    let mut t = virt3();
    t.kid("pl011@9000000").set("compatible", strs(&["arm,sbsa-uart"]));
    assert_eq!(ok(&t).platform.console.map(|c| c.kind), Some(ConsoleKind::SbsaUart));
    assert!(ok(&t).platform.to_string().starts_with("con=sbsa@0x9000000 "));
}

#[test]
fn ns16550_properties_are_validated() {
    let uart = |f: &dyn Fn(&mut N)| {
        let mut t = virt3();
        let u = t.kid("pl011@9000000");
        u.set("compatible", strs(&["ns16550a"]));
        u.set("reg", cells(&[0, 0x900_0000, 0, 0x100]));
        f(u);
        t
    };
    let c = ok(&uart(&|_| {})).platform.console.unwrap();
    assert_eq!(c.kind, ConsoleKind::Ns16550 { reg_shift: 0, io_width: 1 });
    assert_eq!((c.base, c.span), (0x900_0000, 6));

    let c = ok(&uart(&|u| {
        u.set("reg-shift", cells(&[2]));
        u.set("reg-io-width", cells(&[4]));
        u.set("reg-offset", cells(&[0x40]));
    }))
    .platform
    .console
    .unwrap();
    assert_eq!((c.base, c.span), (0x900_0040, 24));

    assert_eq!(
        no_console(&uart(&|u| u.set("reg-io-width", cells(&[2])))),
        NoConsole::BadProperties
    );
    assert_eq!(
        no_console(&uart(&|u| u.set("reg-shift", cells(&[5])))),
        NoConsole::BadProperties
    );
    assert_eq!(
        no_console(&uart(&|u| u.set("reg-shift", vec![0, 2]))),
        NoConsole::BadProperties
    );
    // 레지스터 범위가 선언된 구간을 벗어남
    assert_eq!(
        no_console(&uart(&|u| {
            u.set("reg-shift", cells(&[4]));
            u.set("reg", cells(&[0, 0x900_0000, 0, 0x50]));
        })),
        NoConsole::BadProperties
    );
    assert_eq!(
        no_console(&uart(&|u| u.set("native-endian", flag()))),
        NoConsole::UnsupportedUart
    );
}

#[test]
fn bus_translation_is_strict() {
    // ranges가 없는 버스 아래는 변환할 수 없음
    let mut t = rpi4();
    t.kid("soc").del("ranges");
    assert_eq!(run(&t), Err(DiscoverError::IntcNotTranslatable));

    // 빈 ranges는 identity이고 셀 수가 같아야 함
    let mut t = rpi4();
    t.kid("soc").set("ranges", flag());
    assert_eq!(run(&t), Err(DiscoverError::BadIntcReg));
    let mut t = virt3();
    let bus = N::new("bus")
        .p("#address-cells", cells(&[2]))
        .p("#size-cells", cells(&[2]))
        .p("ranges", flag())
        .c(pl011());
    t.kids.retain(|k| k.name != "pl011@9000000");
    t.kids.push(bus);
    t.kid("chosen").set("stdout-path", strs(&["/bus/pl011@9000000"]));
    assert_eq!(ok(&t).platform.console.unwrap().base, 0x900_0000);

    // 셀 수를 선언하지 않은 버스는 거부(상속하지 않음)
    t.kid("bus").del("#address-cells");
    assert_eq!(no_console(&t), NoConsole::BadProperties);

    // 구간이 ranges 항목 둘에 걸치면 변환 불가
    let mut t = rpi4();
    t.kid("soc")
        .kid("serial@7e215040")
        .set("reg", cells(&[0x7f7f_ffe0, 0x40]));
    t.kid("aliases").set("serial1", strs(&["/soc/serial@7e215040"]));
    assert_eq!(no_console(&t), NoConsole::NotTranslatable);

    // 두 항목이 같은 주소를 덮으면 모호하므로 거부
    let mut t = rpi4();
    t.kid("soc").set(
        "ranges",
        cells(&[
            0x7e00_0000, 0, 0xfe00_0000, 0x180_0000, 0x7e20_0000, 0, 0xee00_0000, 0x10_0000,
            0x4000_0000, 0, 0xff80_0000, 0x80_0000,
        ]),
    );
    assert_eq!(no_console(&t), NoConsole::BadProperties);

    // 부모 주소가 64비트를 넘치면 거부
    let mut t = rpi4();
    t.kid("soc").set(
        "ranges",
        cells(&[0x4000_0000, 0xffff_ffff, 0xffff_0000, 0x80_0000, 0x7e00_0000, 0, 0xfe00_0000, 0x180_0000]),
    );
    assert_eq!(run(&t), Err(DiscoverError::BadIntcReg));

    // ranges 길이가 항목 크기의 배수가 아님
    let mut t = rpi4();
    t.kid("soc").set("ranges", cells(&[0x7e00_0000, 0, 0xfe00_0000]));
    assert_eq!(run(&t), Err(DiscoverError::BadIntcReg));
}

#[test]
fn timer_nodes_are_validated() {
    let mut t = virt3();
    t.kids.retain(|k| k.name != "timer");
    assert_eq!(run(&t), Err(DiscoverError::NoTimer));

    let mut t = virt3();
    t.kids.push(timer(4));
    assert_eq!(run(&t), Err(DiscoverError::MultipleTimers));

    // 꺼진 타이머 노드는 세지 않음
    let mut t = virt3();
    let mut off = timer(4);
    off.name = "timer-off".to_string();
    off.set("status", strs(&["disabled"]));
    t.kids.push(off);
    assert_eq!(ok(&t).platform.timer.intid, 27);

    // armv7 호환 문자열만 있어도 타이머로 봄
    let mut t = virt3();
    t.kid("timer").set("compatible", strs(&["arm,armv7-timer"]));
    assert_eq!(ok(&t).platform.timer.intid, 27);

    for erratum in ["fsl,erratum-a008585", "hisilicon,erratum-161010101", "allwinner,erratum-unknown1"] {
        let mut t = virt3();
        t.kid("timer").set(erratum, flag());
        assert_eq!(run(&t), Err(DiscoverError::TimerErratum), "{erratum}");
    }

    let mut t = virt3();
    t.kid("timer").set("interrupts-extended", cells(&[GIC_PHANDLE, 1, 11, 4]));
    assert_eq!(run(&t), Err(DiscoverError::TimerInterruptsExtended));

    let bad: [(&str, Vec<u8>); 7] = [
        ("empty", Vec::new()),
        ("ragged", cells(&[1, 13, 4, 1, 14])),
        ("only two entries", cells(&[1, 13, 4, 1, 14, 4])),
        ("six entries", cells(&[1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4, 1, 12, 4, 1, 9, 4])),
        ("spi", cells(&[1, 13, 4, 1, 14, 4, 0, 11, 4, 1, 10, 4])),
        ("extended ppi", cells(&[1, 13, 4, 1, 14, 4, 3, 11, 4, 1, 10, 4])),
        ("ppi out of range", cells(&[1, 13, 4, 1, 14, 4, 1, 16, 4, 1, 10, 4])),
    ];
    for (what, value) in bad {
        let mut t = virt3();
        t.kid("timer").set("interrupts", value);
        assert_eq!(run(&t), Err(DiscoverError::BadTimerInterrupts), "{what}");
    }

    let mut t = virt3();
    t.kid("timer").del("interrupts");
    assert_eq!(run(&t), Err(DiscoverError::BadTimerInterrupts));

    let mut t = virt3();
    t.kid("timer").set("interrupt-names", strs(&["sec-phys", "phys", "hyp-phys", "hyp-virt"]));
    assert_eq!(run(&t), Err(DiscoverError::BadTimerInterrupts));

    // 네 셀 지정자의 마지막 셀(PPI 파티션)은 0이어야 함
    let mut t = rk3399();
    t.kid("timer")
        .set("interrupts", cells(&[1, 13, 8, 0, 1, 14, 8, 0, 1, 11, 8, 9, 1, 10, 8, 0]));
    assert_eq!(run(&t), Err(DiscoverError::BadTimerInterrupts));

    let mut t = virt3();
    t.kid("timer").set("clock-frequency", vec![0, 1]);
    assert_eq!(run(&t), Err(DiscoverError::BadTimerClock));
}

#[test]
fn the_interrupt_parent_chain_is_followed_and_bounded() {
    // 타이머 자신의 interrupt-parent가 루트의 것보다 우선함
    let mut t = virt3();
    t.set("interrupt-parent", cells(&[0x1234]));
    t.kid("timer").set("interrupt-parent", cells(&[GIC_PHANDLE]));
    assert_eq!(ok(&t).platform.timer.intid, 27);

    let mut t = virt3();
    t.del("interrupt-parent");
    assert_eq!(run(&t), Err(DiscoverError::NoInterruptParent));

    let mut t = virt3();
    t.set("interrupt-parent", cells(&[0x1234]));
    assert_eq!(run(&t), Err(DiscoverError::IntcNotFound));

    // phandle이 유일하지 않으면 어느 노드인지 고르지 않음
    let mut t = virt3();
    t.kid("pl011@9000000").set("phandle", cells(&[GIC_PHANDLE]));
    assert_eq!(run(&t), Err(DiscoverError::DuplicatePhandle));

    // 옛 이름의 phandle도 받음
    let mut t = virt3();
    let gic = t.kid("intc@8000000");
    gic.del("phandle");
    gic.set("linux,phandle", cells(&[GIC_PHANDLE]));
    assert!(run(&t).is_ok());

    // 조상이 스스로 인터럽트 컨트롤러면 phandle로 가리킬 수 없어 거부
    let mut t = virt3();
    t.del("interrupt-parent");
    let moved = t.kids.pop().unwrap();
    t.kid("intc@8000000").kids.push(moved);
    assert_eq!(run(&t), Err(DiscoverError::UnsupportedInterruptTopology));

    // 형식이 깨진 phandle은 구조 오류
    let mut t = virt3();
    t.kid("pl011@9000000").set("phandle", vec![1, 2]);
    assert_eq!(run(&t), Err(DiscoverError::Dtb(BootError::UnsupportedCells)));
}

#[test]
fn interrupt_controllers_are_validated() {
    let edit = |f: &dyn Fn(&mut N)| {
        let mut t = virt3();
        f(t.kid("intc@8000000"));
        run(&t)
    };
    assert_eq!(edit(&|g| g.set("status", strs(&["disabled"]))), Err(DiscoverError::IntcDisabled));
    assert_eq!(edit(&|g| g.del("interrupt-controller")), Err(DiscoverError::IntcNotController));
    assert_eq!(edit(&|g| g.set("interrupt-map", cells(&[0]))), Err(DiscoverError::IntcInterruptMap));
    assert_eq!(
        edit(&|g| g.set("compatible", strs(&["brcm,bcm2836-l1-intc"]))),
        Err(DiscoverError::UnsupportedIntc)
    );
    assert_eq!(
        edit(&|g| g.set("compatible", strs(&["arm,gic-v5"]))),
        Err(DiscoverError::UnsupportedIntc)
    );
    assert_eq!(
        edit(&|g| g.set("compatible", strs(&["arm,gic-v3", "arm,gic-400"]))),
        Err(DiscoverError::UnsupportedIntc)
    );
    assert_eq!(edit(&|g| g.del("compatible")), Err(DiscoverError::UnsupportedIntc));
    // 벤더 문자열이 앞에 있어도 목록 전체를 봄
    assert!(edit(&|g| g.set("compatible", strs(&["qcom,msm8996-gic-v3", "arm,gic-v3"]))).is_ok());

    assert_eq!(edit(&|g| g.del("#interrupt-cells")), Err(DiscoverError::BadIntcCells));
    assert_eq!(edit(&|g| g.set("#interrupt-cells", cells(&[2]))), Err(DiscoverError::BadIntcCells));
    assert_eq!(edit(&|g| g.set("#interrupt-cells", cells(&[5]))), Err(DiscoverError::BadIntcCells));

    assert_eq!(edit(&|g| g.del("reg")), Err(DiscoverError::BadIntcReg));
    assert_eq!(
        edit(&|g| g.set("reg", cells(&[0, 0x800_0000, 0, 0x1_0000]))),
        Err(DiscoverError::BadIntcReg)
    );
    // 디스트리뷰터는 64KiB 정렬, 64KiB 이상
    assert_eq!(
        edit(&|g| g.set("reg", cells(&[0, 0x800_1000, 0, 0x1_0000, 0, 0x80a_0000, 0, 0xf6_0000]))),
        Err(DiscoverError::BadIntcReg)
    );
    assert_eq!(
        edit(&|g| g.set("reg", cells(&[0, 0x800_0000, 0, 0x1000, 0, 0x80a_0000, 0, 0xf6_0000]))),
        Err(DiscoverError::BadIntcReg)
    );
    assert_eq!(
        edit(&|g| g.set("reg", cells(&[0, 0x800_0000, 0, 0x1_0000, 0, 0x80a_0000, 0, 0x1_0000]))),
        Err(DiscoverError::BadRedistributor)
    );
    assert_eq!(
        edit(&|g| g.set("reg", cells(&[0, 0x800_0000, 0, 0, 0, 0x80a_0000, 0, 0xf6_0000]))),
        Err(DiscoverError::BadIntcReg)
    );
    assert_eq!(
        edit(&|g| g.set("#redistributor-regions", cells(&[0]))),
        Err(DiscoverError::BadRedistributor)
    );
    assert_eq!(
        edit(&|g| g.set("#redistributor-regions", cells(&[5]))),
        Err(DiscoverError::BadRedistributor)
    );
    // 구간 수가 reg 항목보다 많으면 거부
    assert_eq!(
        edit(&|g| g.set("#redistributor-regions", cells(&[2]))),
        Err(DiscoverError::BadIntcReg)
    );
    for stride in [cells(&[0x1_0000]), cells(&[0x2_8000]), vec![0, 2, 0]] {
        assert_eq!(
            edit(&|g| g.set("redistributor-stride", stride.clone())),
            Err(DiscoverError::BadRedistributor)
        );
    }
    let d = edit(&|g| g.set("redistributor-stride", cells(&[0, 0x4_0000]))).unwrap();
    assert!(d.platform.to_string().contains(" stride=0x40000 "));

    // 리디스트리뷰터 구간 둘
    let d = edit(&|g| {
        g.set("#redistributor-regions", cells(&[2]));
        g.set(
            "reg",
            cells(&[
                0, 0x800_0000, 0, 0x1_0000, 0, 0x80a_0000, 0, 0xf6_0000, 0x40, 0, 0, 0x400_0000,
            ]),
        );
    })
    .unwrap();
    assert_eq!(
        d.platform.intc.redist(),
        &[
            Region { base: 0x80a_0000, size: 0xf6_0000 },
            Region { base: 0x40_0000_0000, size: 0x400_0000 }
        ]
    );
    assert!(d.platform.to_string().contains("rd=0x80a0000+0xf60000;0x4000000000+0x4000000 "));

    let edit2 = |f: &dyn Fn(&mut N)| {
        let mut t = virt2();
        f(t.kid("intc@8000000"));
        run(&t)
    };
    assert_eq!(edit2(&|g| g.set("cpu-offset", cells(&[0x8000]))), Err(DiscoverError::NonBankedGic));
    assert_eq!(edit2(&|g| g.set("#interrupt-cells", cells(&[4]))), Err(DiscoverError::BadIntcCells));
    assert_eq!(
        edit2(&|g| g.set("reg", cells(&[0, 0x800_0000, 0, 0x1_0000]))),
        Err(DiscoverError::BadIntcReg)
    );
    assert_eq!(
        edit2(&|g| g.set("reg", cells(&[0, 0x800_0000, 0, 0x1_0000, 0, 0x801_0800, 0, 0x1_0000]))),
        Err(DiscoverError::BadIntcReg)
    );
    for compat in ["arm,gic-400", "arm,cortex-a7-gic"] {
        assert!(edit2(&|g| g.set("compatible", strs(&[compat]))).is_ok(), "{compat}");
    }
    for compat in ["arm,cortex-a9-gic", "arm,pl390", "qcom,msm-qgic2"] {
        assert_eq!(
            edit2(&|g| g.set("compatible", strs(&[compat]))),
            Err(DiscoverError::UnsupportedIntc),
            "{compat}"
        );
    }
}

#[test]
fn structural_rules_apply_to_discovery_too() {
    // 자식 뒤의 프로퍼티
    let mut b = Builder::new();
    b.begin("").begin("chosen").end().prop("interrupt-parent", &cells(&[1])).end();
    assert_eq!(discover_blob(&b.build()), Err(DiscoverError::Dtb(BootError::BadStructure)));

    // 해석 대상 프로퍼티의 중복
    let mut b = Builder::new();
    b.begin("")
        .begin("timer")
        .prop("compatible", &strs(&["arm,armv8-timer"]))
        .prop("compatible", &strs(&["arm,armv7-timer"]))
        .end()
        .end();
    assert_eq!(
        discover_blob(&b.build()),
        Err(DiscoverError::Dtb(BootError::DuplicateProperty))
    );

    // 두 번째 루트
    let mut b = Builder::new();
    b.begin("").end().begin("").end();
    assert_eq!(discover_blob(&b.build()), Err(DiscoverError::Dtb(BootError::BadStructure)));
}

fn layout<'a>(memory: &'a [MemRegion], granule: u64) -> Layout<'a> {
    Layout {
        memory,
        reserved: &[],
        image: 0x4008_0000..0x4030_0000,
        dtb: 0x4800_0000..0x4801_0000,
        granule,
        ram_limit: 1 << 47,
        device_limit: 1 << 40,
    }
}

const RAM: [MemRegion; 1] = [MemRegion { base: 0x4000_0000, size: 0x2000_0000 }];

#[test]
fn validation_accepts_the_qemu_virt_layouts() {
    for tree in [virt3(), virt2()] {
        let p = ok(&tree).platform;
        assert_eq!(p.validate(&layout(&RAM, 4096)), Ok(()));
        assert_eq!(p.validate(&layout(&RAM, 16384)), Ok(()));
    }
    let p = ok(&rpi4()).platform;
    let ram = [MemRegion { base: 0, size: 0x3b40_0000 }];
    let l = Layout {
        image: 0x8_0000..0x30_0000,
        dtb: 0x2e00_0000..0x2e01_0000,
        ..layout(&ram, 16384)
    };
    assert_eq!(p.validate(&l), Ok(()));
}

#[test]
fn validation_rejects_contradictions_with_the_memory_map() {
    let p = ok(&virt3()).platform;

    let mut l = layout(&RAM, 4096);
    l.ram_limit = 0x5000_0000;
    assert_eq!(p.validate(&l), Err(ValidateError::MemoryAboveLimit));

    let mut l = layout(&RAM, 4096);
    l.image = 0x3fff_f000..0x4030_0000;
    assert_eq!(p.validate(&l), Err(ValidateError::ImageOutsideMemory));

    let mut l = layout(&RAM, 4096);
    l.dtb = 0x5fff_f000..0x6000_1000;
    assert_eq!(p.validate(&l), Err(ValidateError::DtbOutsideMemory));

    let mut l = layout(&RAM, 4096);
    l.device_limit = 0x900_0000;
    assert_eq!(p.validate(&l), Err(ValidateError::DeviceAboveLimit));

    // 메모리라고 선언된 구간 안의 디바이스
    let wide = [MemRegion { base: 0, size: 0x6000_0000 }];
    assert_eq!(p.validate(&layout(&wide, 4096)), Err(ValidateError::DeviceOverlapsMemory));

    // 4K로는 떨어져 있지만 16K 페이지로 넓히면 RAM과 같은 페이지에 걸림
    let near = [
        MemRegion { base: 0x900_1000, size: 0x1000 },
        MemRegion { base: 0x4000_0000, size: 0x2000_0000 },
    ];
    assert_eq!(p.validate(&layout(&near, 4096)), Ok(()));
    assert_eq!(p.validate(&layout(&near, 16384)), Err(ValidateError::DeviceOverlapsMemory));

    // 콘솔 구간이 디스트리뷰터와 겹침
    let mut t = virt3();
    t.kid("pl011@9000000").set("reg", cells(&[0, 0x800_8000, 0, 0x1000]));
    t.kid("pl011@9000000").name = "pl011@8008000".to_string();
    t.kid("chosen").set("stdout-path", strs(&["/pl011@8008000"]));
    let p = ok(&t).platform;
    assert_eq!(p.validate(&layout(&RAM, 4096)), Err(ValidateError::DevicesOverlap));

    // 레지스터 범위가 페이지 경계를 넘는 콘솔
    let mut t = virt3();
    let u = t.kid("pl011@9000000");
    u.set("compatible", strs(&["ns16550a"]));
    u.set("reg", cells(&[0, 0x900_0ff0, 0, 0x100]));
    u.set("reg-shift", cells(&[2]));
    let p = ok(&t).platform;
    assert_eq!(p.validate(&layout(&RAM, 4096)), Err(ValidateError::ConsoleCrossesPage));
    assert_eq!(p.validate(&layout(&RAM, 16384)), Ok(()));
}

fn invariants(d: &Discovery) {
    let p = &d.platform;
    assert_eq!(p.console.is_none(), d.no_console.is_some());
    assert!((16..=31).contains(&p.timer.intid));
    if let Some(c) = &p.console {
        assert!(c.span > 0 && c.reg.size > 0);
        assert!(c.base >= c.reg.base && c.base + c.span <= c.reg.base + c.reg.size);
        assert!(c.reg.base.checked_add(c.reg.size).is_some());
    }
    let check = |r: &Region| {
        assert!(r.size > 0, "{r:?}");
        assert!(r.base.checked_add(r.size).is_some(), "{r:?}");
    };
    match &p.intc {
        Intc::GicV2 { dist, cpu } => {
            check(dist);
            check(cpu);
        }
        Intc::GicV3 { dist, redist_count, stride, .. } => {
            check(dist);
            assert!((1..=MAX_REDIST_REGIONS).contains(redist_count));
            p.intc.redist().iter().for_each(check);
            assert!(*stride == 0 || (*stride >= 0x2_0000 && stride % 0x1_0000 == 0));
        }
    }
    // 형식화가 패닉 없이 끝나야 함
    assert!(!p.to_string().is_empty());
}

#[test]
fn fuzz_discover_with_mutated_trees() {
    let only = env_u64("K0_FUZZ_CASE");
    let base = env_u64("K0_FUZZ_SEED").unwrap_or(0x6B30_5F50_4C41_5446);
    let cases = if only.is_some() { 1 } else { env_u64("K0_FUZZ_ITERS").unwrap_or(20_000) };
    let seeds = [virt3().blob(), virt2().blob(), rpi4().blob(), rk3399().blob()];
    let (mut accepted, mut rejected) = (0u64, 0u64);

    // 기준 시드를 먼저 섞어 이웃한 기준 시드끼리 케이스 열이 겹치지 않게 함
    let stream = splitmix(base);
    for i in 0..cases {
        let seed = only.unwrap_or_else(|| splitmix(stream.wrapping_add(i)));
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut rng = Rng::new(seed);
            let mut blob = seeds[rng.below(seeds.len() as u64) as usize].clone();
            mutate(&mut rng, &mut blob);
            match discover_blob(&blob) {
                Ok(d) => {
                    invariants(&d);
                    true
                }
                Err(_) => false,
            }
        }));
        match r {
            Ok(true) => accepted += 1,
            Ok(false) => rejected += 1,
            Err(_) => panic!("fuzz case failed, reproduce with K0_FUZZ_CASE={seed:#x} (case {i}, base seed {base:#x})"),
        }
    }
    eprintln!("fuzz: {cases} discovery cases, accepted {accepted}, rejected {rejected}");
    if only.is_none() && cases >= 1000 {
        assert!(accepted > 0, "no mutated tree was ever accepted");
        assert!(rejected > 0, "no mutated tree was ever rejected");
    }
}

/// 실제 DTB 파일에 발견을 돌려 결과를 찍는 도구입니다. 평소에는 건너뜁니다.
///
/// `K0_DTB_DIR=<디렉터리> cargo test -p k0-boot probe_dtbs -- --ignored --nocapture`
/// 처럼 부르면 그 안의 `.dtb` 파일마다 커널이 볼 플랫폼 줄을 출력합니다. 새
/// 보드의 DTB를 부팅 전에 확인하거나 고정 모드에 넣을 줄을 얻는 용도입니다.
#[test]
#[ignore = "needs K0_DTB_DIR"]
fn probe_dtbs_from_env() {
    let dir = std::env::var("K0_DTB_DIR").expect("set K0_DTB_DIR to a directory of .dtb files");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("readable directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "dtb"))
        .collect();
    paths.sort();
    for path in paths {
        let blob = std::fs::read(&path).expect("readable file");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // 덤프된 DTB는 뒤에 여백이 붙어 있을 수 있어 헤더의 크기만큼만 봄
        let total = blob.get(4..8).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
        let blob = match total {
            Some(n) if n <= blob.len() => &blob[..n],
            _ => &blob[..],
        };
        match discover_blob(blob) {
            Ok(d) => match d.no_console {
                None => println!("{name}: {}", d.platform),
                Some(why) => println!("{name}: {} ({why:?})", d.platform),
            },
            Err(e) => println!("{name}: refused, {e:?}"),
        }
        match crate::fdt::tests_support::parse_for_probe(blob) {
            Ok((mem, rsv)) => println!("{name}:   memory {mem:x?} reserved {rsv:x?}"),
            Err(e) => println!("{name}:   memory map refused, {e:?}"),
        }
    }
}

#[test]
fn devices_must_not_sit_in_reserved_regions() {
    let p = ok(&virt3()).platform;
    let rsv = [MemRegion { base: 0x900_0000, size: 0x100 }];
    let mut l = layout(&RAM, 4096);
    l.reserved = &rsv;
    assert_eq!(p.validate(&l), Err(ValidateError::DeviceOverlapsMemory));
}

#[test]
fn the_console_can_be_found_without_the_rest() {
    // 인터럽트 컨트롤러가 지원 밖이어도 콘솔은 따로 찾을 수 있음
    let mut t = virt3();
    t.kid("intc@8000000").set("compatible", strs(&["brcm,bcm2836-l1-intc"]));
    assert_eq!(run(&t), Err(DiscoverError::UnsupportedIntc));
    let c = console_of(&t.blob()).expect("console");
    assert_eq!(c.base, 0x900_0000);
    assert_eq!(c.check(&layout(&RAM, 4096)), Ok(()));
    t.kid("chosen").del("stdout-path");
    assert_eq!(console_of(&t.blob()), None);
    assert_eq!(console_of(&[0u8; 64]), None);
}

#[test]
fn console_register_accesses_must_be_aligned() {
    let uart = |f: &dyn Fn(&mut N)| {
        let mut t = virt3();
        let u = t.kid("pl011@9000000");
        u.set("compatible", strs(&["ns16550a"]));
        u.set("reg", cells(&[0, 0x900_0000, 0, 0x100]));
        f(u);
        t
    };
    // 32비트 접근인데 레지스터 간격이 1바이트나 2바이트면 5번 레지스터가 어긋남
    for shift in [0, 1] {
        assert_eq!(
            no_console(&uart(&|u| {
                u.set("reg-io-width", cells(&[4]));
                u.set("reg-shift", cells(&[shift]));
            })),
            NoConsole::BadProperties
        );
    }
    assert_eq!(
        no_console(&uart(&|u| {
            u.set("reg-io-width", cells(&[4]));
            u.set("reg-shift", cells(&[2]));
            u.set("reg-offset", cells(&[2]));
        })),
        NoConsole::BadProperties
    );
    assert!(ok(&uart(&|u| {
        u.set("reg-io-width", cells(&[4]));
        u.set("reg-shift", cells(&[2]));
    }))
    .platform
    .console
    .is_some());
    // 바이트 접근은 어떤 간격이든 정렬 조건이 없음
    assert!(ok(&uart(&|u| u.set("reg-offset", cells(&[3])))).platform.console.is_some());

    let mut t = virt3();
    t.kid("pl011@9000000").set("reg", cells(&[0, 0x900_0002, 0, 0x1000]));
    assert_eq!(no_console(&t), NoConsole::BadProperties);
}
