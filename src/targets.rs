//! Cross-compilation target specifications.
//!
//! The native backend normally emits a host executable with the system C
//! compiler.  Bare-metal targets need a freestanding compiler, architecture
//! flags, a startup stub, and a linker script instead.  This module keeps those
//! details in one place so the CLI, the C backend, and the integration tests
//! agree on the exact toolchain contract.
//!
//! The RISC-V `virt` targets mirror Renode's `platforms/cpus/riscv_virt.repl`:
//! RAM starts at `0x80000000` and an NS16550 UART is mapped at `0x10000000`.
//! The platform descriptions, startup stub, linker script, and Renode monitor
//! templates live in the repository's `renode/` directory and are embedded in
//! the binary.

/// Target families understood by the build pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// The machine running `lusalang`; uses the host C compiler.
    Host,
    /// 32-bit RISC-V `virt` machine emulated by Renode.
    RiscV32Virt,
    /// 64-bit RISC-V `virt` machine emulated by Renode.
    RiscV64Virt,
    /// Raspberry Pi RP2040 (dual Cortex-M0+).
    Rp2040,
    /// STM32F4 family (Cortex-M4).
    Stm32F4,
    /// Espressif ESP32-C3 (RISC-V rv32imc).
    Esp32C3,
}

/// A complete cross-compilation recipe.
#[derive(Clone, Copy, Debug)]
pub struct TargetSpec {
    /// CLI spelling, for example `riscv32-virt`.
    pub name: &'static str,
    /// Conventional GNU triple used in diagnostics and docs.
    pub triple: &'static str,
    pub kind: TargetKind,
    /// Toolchain family shown in compiler diagnostics, for example `GNU RISC-V`.
    pub toolchain: &'static str,
    /// One concrete toolchain distribution that provides a matching compiler.
    pub toolchain_hint: &'static str,
    /// Compiler executable names tried on `PATH`, in order.
    pub compiler_candidates: &'static [&'static str],
    /// Environment variables that may point at a compiler, in order.
    pub compiler_env: &'static [&'static str],
    /// Architecture and ABI flags passed to compilation and linking.
    pub arch_flags: &'static [&'static str],
    /// Default filename extension for the linked image.
    pub output_extension: &'static str,
    /// Freestanding linker script (empty for host targets).
    pub linker_script: &'static str,
    /// Reset-vector/startup assembly (empty for host targets).
    pub startup: &'static str,
    /// Renode platform description (empty for host targets).
    pub renode_platform: &'static str,
    /// Renode monitor script template (empty for host targets).
    pub renode_script: &'static str,
}

impl TargetSpec {
    pub fn is_host(&self) -> bool {
        self.kind == TargetKind::Host
    }
}

/// The host target.  Its toolchain details are resolved by the existing host
/// compiler discovery code, so the cross-specific fields stay empty.
pub static HOST: TargetSpec = TargetSpec {
    name: "host",
    triple: "",
    kind: TargetKind::Host,
    toolchain: "host",
    toolchain_hint: "the system C compiler",
    compiler_candidates: &[],
    compiler_env: &[],
    arch_flags: &[],
    output_extension: "",
    linker_script: "",
    startup: "",
    renode_platform: "",
    renode_script: "",
};

/// Renode's 32-bit RISC-V `virt` machine with an NS16550 UART.
pub static RISCV32_VIRT: TargetSpec = TargetSpec {
    name: "riscv32-virt",
    triple: "riscv32-unknown-elf",
    kind: TargetKind::RiscV32Virt,
    toolchain: "GNU RISC-V",
    toolchain_hint: "for example xPack riscv-none-elf-gcc",
    compiler_candidates: &[
        "riscv32-unknown-elf-gcc",
        "riscv-none-elf-gcc",
        "riscv64-unknown-elf-gcc",
        "riscv64-elf-gcc",
    ],
    compiler_env: &["LUSA_RISCV_CC", "LUSA_CROSS_CC"],
    arch_flags: &[
        "-march=rv32imac",
        "-mabi=ilp32",
        "-mcmodel=medany",
    ],
    output_extension: "elf",
    linker_script: include_str!("../renode/linker_riscv.ld"),
    startup: include_str!("../renode/startup_riscv.S"),
    renode_platform: include_str!("../renode/lusa_riscv32_virt.repl"),
    renode_script: include_str!("../renode/lusa_riscv32_virt.resc"),
};

/// Renode's 64-bit RISC-V `virt` machine with an NS16550 UART.
pub static RISCV64_VIRT: TargetSpec = TargetSpec {
    name: "riscv64-virt",
    triple: "riscv64-unknown-elf",
    kind: TargetKind::RiscV64Virt,
    toolchain: "GNU RISC-V",
    toolchain_hint: "for example xPack riscv-none-elf-gcc",
    compiler_candidates: &[
        "riscv64-unknown-elf-gcc",
        "riscv-none-elf-gcc",
        "riscv64-elf-gcc",
        "riscv32-unknown-elf-gcc",
    ],
    compiler_env: &["LUSA_RISCV_CC", "LUSA_CROSS_CC"],
    arch_flags: &[
        "-march=rv64imac",
        "-mabi=lp64",
        "-mcmodel=medany",
    ],
    output_extension: "elf",
    linker_script: include_str!("../renode/linker_riscv.ld"),
    startup: include_str!("../renode/startup_riscv.S"),
    renode_platform: include_str!("../renode/lusa_riscv64_virt.repl"),
    renode_script: include_str!("../renode/lusa_riscv64_virt.resc"),
};

/// Raspberry Pi RP2040: dual Cortex-M0+, 2 MiB flash, 264 KiB SRAM.
pub static RP2040: TargetSpec = TargetSpec {
    name: "rp2040",
    triple: "thumbv6m-none-eabi",
    kind: TargetKind::Rp2040,
    toolchain: "GNU Arm Embedded",
    toolchain_hint: "for example xPack arm-none-eabi-gcc",
    compiler_candidates: &["arm-none-eabi-gcc"],
    compiler_env: &["LUSA_ARM_CC", "LUSA_CROSS_CC"],
    arch_flags: &[
        "-mcpu=cortex-m0plus",
        "-mthumb",
        "-mfloat-abi=soft",
    ],
    output_extension: "elf",
    linker_script: include_str!("../renode/linker_arm_rp2040.ld"),
    startup: include_str!("../renode/startup_arm.S"),
    renode_platform: include_str!("../renode/lusa_rp2040.repl"),
    renode_script: include_str!("../renode/lusa_rp2040.resc"),
};

/// STM32F4: Cortex-M4, 1 MiB flash, 128 KiB SRAM.
pub static STM32F4: TargetSpec = TargetSpec {
    name: "stm32f4",
    triple: "thumbv7em-none-eabi",
    kind: TargetKind::Stm32F4,
    toolchain: "GNU Arm Embedded",
    toolchain_hint: "for example xPack arm-none-eabi-gcc",
    compiler_candidates: &["arm-none-eabi-gcc"],
    compiler_env: &["LUSA_ARM_CC", "LUSA_CROSS_CC"],
    arch_flags: &[
        "-mcpu=cortex-m4",
        "-mthumb",
        "-mfloat-abi=soft",
    ],
    output_extension: "elf",
    linker_script: include_str!("../renode/linker_arm_stm32f4.ld"),
    startup: include_str!("../renode/startup_arm.S"),
    renode_platform: include_str!("../renode/lusa_stm32f4.repl"),
    renode_script: include_str!("../renode/lusa_stm32f4.resc"),
};

/// Espressif ESP32-C3: RV32IMC, 4 MiB flash, 400 KiB SRAM.
pub static ESP32C3: TargetSpec = TargetSpec {
    name: "esp32c3",
    triple: "riscv32-esp-elf",
    kind: TargetKind::Esp32C3,
    toolchain: "GNU RISC-V",
    toolchain_hint: "for example xPack riscv-none-elf-gcc",
    compiler_candidates: &[
        "riscv32-esp-elf-gcc",
        "riscv-none-elf-gcc",
        "riscv32-unknown-elf-gcc",
        "riscv64-unknown-elf-gcc",
    ],
    compiler_env: &["LUSA_ESP32_CC", "LUSA_RISCV_CC", "LUSA_CROSS_CC"],
    arch_flags: &[
        "-march=rv32imc",
        "-mabi=ilp32",
        "-mcmodel=medany",
    ],
    output_extension: "elf",
    linker_script: include_str!("../renode/linker_riscv_flash.ld"),
    startup: include_str!("../renode/startup_riscv.S"),
    renode_platform: include_str!("../renode/lusa_esp32c3.repl"),
    renode_script: include_str!("../renode/lusa_esp32c3.resc"),
};

/// Every non-host target offered by `lusalang build --target`.
pub static CROSS_TARGETS: &[&TargetSpec] =
    &[&RISCV32_VIRT, &RISCV64_VIRT, &RP2040, &STM32F4, &ESP32C3];

/// Resolves a CLI target name or triple.  `host`, `native`, and the empty
/// string select the host target.
pub fn find(name: &str) -> Option<&'static TargetSpec> {
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("native")
    {
        return Some(&HOST);
    }
    CROSS_TARGETS
        .iter()
        .copied()
        .find(|target| target.name.eq_ignore_ascii_case(name) || target.triple.eq_ignore_ascii_case(name))
}

/// Human-readable list for diagnostics and `--help`.
pub fn target_names() -> String {
    let mut names = vec!["host".to_string()];
    names.extend(CROSS_TARGETS.iter().map(|target| target.name.to_string()));
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_host_and_cross_names() {
        assert!(find("host").unwrap().is_host());
        assert!(find("").unwrap().is_host());
        assert_eq!(find("riscv32-virt").unwrap().kind, TargetKind::RiscV32Virt);
        assert_eq!(
            find("riscv64-unknown-elf").unwrap().kind,
            TargetKind::RiscV64Virt
        );
        assert!(find("mips").is_none());
    }

    #[test]
    fn riscv_assets_are_embedded() {
        for target in [&RISCV32_VIRT, &RISCV64_VIRT] {
            assert!(target.linker_script.contains("0x80000000"));
            assert!(target.startup.contains("lusa_entry"));
            assert!(target.renode_platform.contains("UART.NS16550 @ sysbus 0x10000000"));
            assert!(target.renode_script.contains("CreateFileBackend"));
            assert!(target.output_extension == "elf");
        }
    }

    #[test]
    fn resolves_modern_mcu_names_and_architecture_triples() {
        assert_eq!(find("rp2040").unwrap().kind, TargetKind::Rp2040);
        assert_eq!(find("thumbv6m-none-eabi").unwrap().kind, TargetKind::Rp2040);
        assert_eq!(find("stm32f4").unwrap().kind, TargetKind::Stm32F4);
        assert_eq!(find("thumbv7em-none-eabi").unwrap().kind, TargetKind::Stm32F4);
        assert_eq!(find("esp32c3").unwrap().kind, TargetKind::Esp32C3);
        assert_eq!(find("riscv32-esp-elf").unwrap().kind, TargetKind::Esp32C3);
        // The RISC-V `virt` triple still selects the emulated board, not the MCU.
        assert_eq!(
            find("riscv32-unknown-elf").unwrap().kind,
            TargetKind::RiscV32Virt
        );
    }

    #[test]
    fn modern_mcu_assets_match_their_memory_maps() {
        assert!(RP2040.linker_script.contains("ORIGIN = 0x10000000"));
        assert!(RP2040.linker_script.contains("LENGTH = 2M"));
        assert!(RP2040.linker_script.contains("ORIGIN = 0x20000000"));
        assert!(RP2040.linker_script.contains("LENGTH = 264K"));
        assert!(RP2040.renode_platform.contains("cpuType: \"cortex-m0\""));
        assert!(RP2040.renode_platform.contains("UART.PL011 @ sysbus 0x40034000"));
        assert!(RP2040.renode_platform.contains("systickFrequency: 125000000"));
        assert!(RP2040.renode_platform.contains("Memory.MappedMemory @ sysbus 0xD0000000"));

        assert!(STM32F4.linker_script.contains("ORIGIN = 0x08000000"));
        assert!(STM32F4.linker_script.contains("LENGTH = 1M"));
        assert!(STM32F4.renode_platform.contains("cpuType: \"cortex-m4\""));
        assert!(STM32F4.renode_platform.contains("UART.STM32_UART"));
        assert!(STM32F4.renode_platform.contains("GPIOPort.STM32_GPIOPort"));
        assert!(STM32F4.renode_platform.contains("Timers.STM32_Timer"));

        assert!(ESP32C3.linker_script.contains("ORIGIN = 0x42000000"));
        assert!(ESP32C3.linker_script.contains("LENGTH = 4M"));
        assert!(ESP32C3.linker_script.contains("ORIGIN = 0x3FC80000"));
        assert!(ESP32C3.linker_script.contains("LENGTH = 400K"));
        assert!(ESP32C3.renode_platform.contains("CPU.RiscV32"));
        assert!(ESP32C3.renode_platform.contains("rv32imc"));
        assert!(ESP32C3.renode_platform.contains("UART.ESP32_UART"));
        assert!(ESP32C3.renode_platform.contains("Timers.Arm_CMSDK_Timer"));

        for target in CROSS_TARGETS {
            assert!(target.startup.contains("lusa_entry"));
            assert!(target.renode_script.contains("CreateFileBackend"));
            assert!(target.renode_script.contains("sysbus LoadELF @ELF@"));
            assert_eq!(target.output_extension, "elf");
            assert!(!target.toolchain.is_empty());
            assert!(!target.toolchain_hint.is_empty());
        }
    }
}
