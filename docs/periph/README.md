<!-- generated from specs/*.toml by `cargo run -- spec-docs --update` – do not edit -->

# Peripheral register specs

Generated from the TOML specs in [`specs/`](../../specs/); see [`specs/README.md`](../../specs/README.md) for the format and the rules.

| Block | Bus | Base | Size | Registers | Summary |
|---|---|---|---|---|---|
| [`armctrl`](armctrl.md) | vpu | `0x7E00B000` | `0x880` | 12 | ARM control block as the VPU sees it, below the mailboxes: where `arm_loader` releases the ARM |
| [`armlocal`](armlocal.md) | arm | `0xFF800000` | `0x100` | 2 | ARM local block: only the two registers the armstub writes |
| [`asb`](asb.md) | vpu | `0x7E00A000` | `0x1000` | 4 | AXI async slave bridges: the stop / acknowledge handshake before gating the V3D, ISP and H264 power domains |
| [`aux`](aux.md) | vpu | `0x7E215000` | `0x100` | 25 | AUX: mini-UART (UART1) and the SPI1 / SPI2 masters |
| [`avs`](avs.md) | vpu | `0x7D5D2000` | `0xF00` | 10 | AVS monitor: on-die temperature sensor and the ring-oscillator / rail voltage monitors |
| [`bcm54213pe`](bcm54213pe.md) | mdio | `0x01` | `0x20` | 23 | BCM54213PE gigabit Ethernet PHY on GENET's MDIO bus |
| [`bell`](bell.md) | vpu | `0x7E00B840` | `0x10` | 1 | The four ARM <-> VideoCore doorbells, VCHIQ's wake path |
| [`bootbox`](bootbox.md) | vpu | `0x7EE00000` | `0x4000` | 20 | Boot-info handoff doorbells, and the VPU interrupt window start4's exception-12 handler reads |
| [`bsc`](bsc.md) | vpu | `0x7E205000` | `0x20` | 8 | BSC (I²C master): instance 0 with nothing attached, and the instance the board PMICs and GPIO expander sit on |
| [`clkmon`](clkmon.md) | vpu | `0x7D5D0000` | `0x10000` | 6 | VPU clock block (PLLs and frequency monitors) below the `0x7E` window |
| [`cm`](cm.md) | vpu | `0x7E101000` | `0x2000` | 93 | Clock manager, with the A2W PLL control in the same window |
| [`corectl`](corectl.md) | vpu | `0x7E002000` | `0x1000` | 10 | VPU core control: per-core boot handshake and interrupt controller |
| [`dma`](dma.md) | vpu | `0x7E007000` | `0x1000` | 11 | Legacy DMA controller: 15 channels `0x100` apart plus the controller-wide interrupt status and enable words |
| [`dma4`](dma4.md) | vpu | `0x7E007B00` | `0x100` | 4 | DMA4 (`dma40`) channel: the 40-bit DMA engine the bootloader and start4 use |
| [`dma_vpu`](dma_vpu.md) | vpu | `0x7EE04100` | `0x1000` | 9 | The DMA controller start4's dmalib drives: 16 channel slots, channel 15 at `0x7EE05000` |
| [`dwc2`](dwc2.md) | vpu | `0x7E980000` | `0x10000` | 17 | DesignWare USB 2.0 OTG controller (the USB-C port): the reset start4 runs when USB power comes on, and a host port with nothing plugged in |
| [`emmc`](emmc.md) | vpu | `0x7E300000` | `0x100` | 20 | The legacy EMMC controller (Arasan SDHCI): the Pi 4's WiFi SDIO host, and the host 2020-era bootcode reads the SD card through |
| [`emmc2`](emmc2.md) | vpu | `0x7E340000` | `0x1000` | 24 | EMMC2: the SD host controller (Arasan SDHCI 3.00) the bootloader, start4 and Linux boot from |
| [`fxl6408`](fxl6408.md) | i2c | `0x43` | `0x100` | 10 | FXL6408 GPIO expander: the board's 'external' GPIOs 128..135 |
| [`genet`](genet.md) | vpu | `0x7D580000` | `0x10000` | 75 | GENET v5 Ethernet MAC with its UniMAC MDIO controller (the PHY is `bcm54213pe`) |
| [`gicc`](gicc.md) | arm | `0xFF842000` | `0x2000` | 15 | GIC-400 CPU interface, banked per CPU and per security state |
| [`gicd`](gicd.md) | arm | `0xFF841000` | `0x1000` | 16 | GIC-400 distributor |
| [`gich`](gich.md) | arm | `0xFF844000` | `0x2000` | 10 | GIC-400 virtual interface control, banked per CPU |
| [`gicv`](gicv.md) | arm | `0xFF846000` | `0x2000` | 0 | GIC-400 virtual CPU interface |
| [`gpio`](gpio.md) | vpu | `0x7E200000` | `0x1000` | 16 | The 58 GPIO pins: function select, output latch, pin levels, edge detect and the BCM2711 pull control |
| [`hd`](hd.md) | vpu | `0x7E808000` | `0x100` | 2 | Control block at `0x7E80_8000`: the power request and acknowledge start4 waits on before it resets the DWC2 USB controller |
| [`hdmi`](hdmi.md) | vpu | `0x7EF00700` | `0x300` | 4 | HDMI controller core registers (the `hdmi` window of each connector), with no monitor attached: the packet-RAM handshake, the FIFO recenter and the hotplug state |
| [`hdmi_auto_i2c`](hdmi_auto_i2c.md) | vpu | `0x7EF00B00` | `0x300` | 5 | HDMI auto-i2c sequencers (the second reg window of each DDC master's node): channels that write a list of values into their connector's DDC I²C master and report when the transfer it starts has finished |
| [`hdmi_ddc`](hdmi_ddc.md) | vpu | `0x7EF04500` | `0x100` | 8 | HDMI DDC I²C masters (`brcm,bcm2711-hdmi-i2c`), one per connector: the bus a monitor's EDID EEPROM sits on |
| [`hvs`](hvs.md) | vpu | `0x7E400000` | `0x1000` | 7 | HVS (Hardware Video Scaler): identification, the per-channel frame-swap words, and end of frame |
| [`mbox`](mbox.md) | vpu | `0x7E00B880` | `0x140` | 10 | ARM <-> VideoCore mailboxes: two views of the same pair of FIFOs, and the interrupt block between them |
| [`mcsync`](mcsync.md) | vpu | `0x7E000000` | `0x1000` | 13 | Thirty-two hardware semaphores, plus the mailbox words and ack registers around them |
| [`otp`](otp.md) | vpu | `0x7E20F000` | `0x1000` | 7 | Always-on config / OTP engine: the fuse array, one row at a time – reads and programming |
| [`pactl`](pactl.md) | vpu | `0x7E204E00` | `0x4` | 1 | Peripheral activity status: which SPI, I²C or PL011 behind an ORed interrupt is the one asking |
| [`pcie`](pcie.md) | vpu | `0x7D500000` | `0x9310` | 43 | PCIe root complex (`pcie-brcmstb`), with the VL805 xHCI controller behind it |
| [`pcm`](pcm.md) | vpu | `0x7E203000` | `0x24` | 9 | PCM / I²S audio: the one serial-audio interface on the chip |
| [`pm`](pm.md) | vpu | `0x7E100000` | `0x1000` | 19 | Power management: reset control, reset status, watchdog, power-domain registers |
| [`pmic_1d`](pmic_1d.md) | i2c | `0x1D` | `0x100` | 8 | Board PMIC at `0x1D` on every Pi 4-family board but the 4B rev 1.5 (start4 descriptor type `0x81`) |
| [`pmic_core`](pmic_core.md) | i2c | `0x1E` | `0x100` | 4 | Board PMIC owning the SoC core rail (start4 descriptor type `0x82`) |
| [`pmic_rails`](pmic_rails.md) | i2c | `0x1B` | `0x100` | 6 | Board PMIC owning the SDRAM and I/O rails (start4 descriptor type `0x83`) |
| [`pvt`](pvt.md) | vpu | `0x7D5D8000` | `0x480` | 5 | Per-channel PVT (process / voltage / temperature) monitors |
| [`pwm`](pwm.md) | vpu | `0x7E20C000` | `0x28` | 8 | PWM0: two pulse-width / serialiser channels and the FIFO they share |
| [`rng`](rng.md) | vpu | `0x7E104000` | `0x28` | 10 | Hardware RNG (RNG200): generator control, warm-up counter, interrupt and FIFO |
| [`sdc`](sdc.md) | vpu | `0x7E001000` | `0x1000` | 4 | Legacy SDRAM-controller interface: DRAM timing table, sub-controller ready bits, LPDDR4 mode-register port |
| [`sdramc`](sdramc.md) | vpu | `0x7DC00000` | `0x40000` | 13 | LPDDR4 controller and PHY, below the `0x7E` window |
| [`spi0`](spi0.md) | vpu | `0x7E204000` | `0x18` | 6 | SPI0 master, with the serial-NOR flash the EEPROM bootloader was loaded from |
| [`systimer`](systimer.md) | vpu | `0x7E003000` | `0x1000` | 4 | System timer: 64-bit free-running 1 MHz counter with four compare channels |
| [`uart0`](uart0.md) | vpu | `0x7E201000` | `0x1000` | 11 | PL011 UART0: the firmware's debug console and Linux's `ttyAMA0` |
| [`vce`](vce.md) | vpu | `0x7F100000` | `0x21000` | 3 | VCE vector/codec engine: data memory, program memory and register file |
| [`vce_ctrl`](vce_ctrl.md) | vpu | `0x7F140000` | `0x1000` | 6 | VCE control block: status, launch, interrupt clear and endcode enables |
| [`vl805`](vl805.md) | pci | `0x00000000` | `0x1000` | 27 | VIA VL805 xHCI controller (`1106:3483`): its PCI configuration space |
| [`xhci`](xhci.md) | pci | `0x00000000` | `0x1000` | 39 | xHCI register block behind the VL805's BAR0: capability, operational, runtime and doorbell registers |
| [`xhci_otg`](xhci_otg.md) | vpu | `0x7E9C0000` | `0x100000` | 35 | The BCM2711's own xHCI controller: the USB-C port as a USB 2.0 host, which the bootloader boots from as `BCM-USB-MSD` and Linux uses with `otg_mode=1` |

## What carries what

The `parent` of a block: the master of its bus, or the window it is carved out of and decoded ahead of. Drawn by hand in [`board-sheet.svg`](../board-sheet.svg), which `tests/board_sheet.rs` checks against these specs.

- [`bsc`](bsc.md)
  - [`fxl6408`](fxl6408.md) — i2c `0x43`, `PMIC` copy
  - [`pmic_1d`](pmic_1d.md) — i2c `0x1D`, `PMIC` copy
  - [`pmic_core`](pmic_core.md) — i2c `0x1E`, `PMIC` copy
  - [`pmic_rails`](pmic_rails.md) — i2c `0x1B`, `PMIC` copy
- [`clkmon`](clkmon.md)
  - [`avs`](avs.md) — vpu `0x7D5D2000`
  - [`pvt`](pvt.md) — vpu `0x7D5D8000`
- [`dma`](dma.md)
  - [`dma4`](dma4.md) — vpu `0x7E007B00`
- [`genet`](genet.md)
  - [`bcm54213pe`](bcm54213pe.md) — mdio `0x01`
- [`pcie`](pcie.md)
  - [`vl805`](vl805.md) — pci `0x00000000`
    - [`xhci`](xhci.md) — pci `0x00000000`

## Interrupt lines

| Controller | Number | Block | Device tree |
|---|---|---|---|
| VPU source | 64 | [`systimer`](systimer.md) `C0` | — |
| VPU source | 65 | [`systimer`](systimer.md) `C1` | — |
| VPU source | 66 | [`systimer`](systimer.md) `C2` | — |
| VPU source | 67 | [`systimer`](systimer.md) `C3` | — |
| VPU source | 68 | [`vce_ctrl`](vce_ctrl.md) | — |
| VPU source | 73 | [`dwc2`](dwc2.md) | — |
| VPU source | 76 | [`mcsync`](mcsync.md) `ACK76` | — |
| VPU source | 77 | [`mcsync`](mcsync.md) `ACK77` | — |
| VPU source | 80 | [`dma`](dma.md) `CH0` | — |
| VPU source | 81 | [`dma`](dma.md) `CH1` | — |
| VPU source | 82 | [`dma`](dma.md) `CH2` | — |
| VPU source | 83 | [`dma`](dma.md) `CH3` | — |
| VPU source | 84 | [`dma`](dma.md) `CH4` | — |
| VPU source | 85 | [`dma`](dma.md) `CH5` | — |
| VPU source | 86 | [`dma`](dma.md) `CH6` | — |
| VPU source | 87 | [`dma`](dma.md) `CH7_8` | — |
| VPU source | 88 | [`dma`](dma.md) `CH9_10` | — |
| VPU source | 89 | [`dma4`](dma4.md) | — |
| VPU source | 93 | [`aux`](aux.md) | — |
| VPU source | 94 | [`bell`](bell.md) | — |
| VPU source | 94 | [`mbox`](mbox.md) | — |
| VPU source | 95 | [`dma`](dma.md) `CH15` | — |
| VPU source | 97 | [`hvs`](hvs.md) | — |
| VPU source | 113 | [`gpio`](gpio.md) `BANK0` | — |
| VPU source | 114 | [`gpio`](gpio.md) `BANK1` | — |
| VPU source | 115 | [`gpio`](gpio.md) `BANK1_MIRROR` | — |
| VPU source | 116 | [`gpio`](gpio.md) `ANY` | — |
| VPU source | 117 | [`bsc`](bsc.md) | — |
| VPU source | 118 | [`spi0`](spi0.md) | — |
| VPU source | 119 | [`pcm`](pcm.md) | — |
| VPU source | 121 | [`uart0`](uart0.md) | — |
| VPU source | 125 | [`rng`](rng.md) | — |
| VPU source | 126 | [`emmc2`](emmc2.md) | — |
| VPU source | 126 | [`emmc`](emmc.md) | — |
| GIC id | 65 | [`mbox`](mbox.md) | `GIC_SPI 33` |
| GIC id | 66 | [`bell`](bell.md) `DOORBELL0` | `GIC_SPI 34` |
| GIC id | 67 | [`bell`](bell.md) `DOORBELL1` | `GIC_SPI 35` |
| GIC id | 105 | [`dwc2`](dwc2.md) | `GIC_SPI 73` |
| GIC id | 112 | [`dma`](dma.md) `CH0` | `GIC_SPI 80` |
| GIC id | 113 | [`dma`](dma.md) `CH1` | `GIC_SPI 81` |
| GIC id | 114 | [`dma`](dma.md) `CH2` | `GIC_SPI 82` |
| GIC id | 115 | [`dma`](dma.md) `CH3` | `GIC_SPI 83` |
| GIC id | 116 | [`dma`](dma.md) `CH4` | `GIC_SPI 84` |
| GIC id | 117 | [`dma`](dma.md) `CH5` | `GIC_SPI 85` |
| GIC id | 118 | [`dma`](dma.md) `CH6` | `GIC_SPI 86` |
| GIC id | 119 | [`dma`](dma.md) `CH7_8` | `GIC_SPI 87` |
| GIC id | 120 | [`dma`](dma.md) `CH9_10` | `GIC_SPI 88` |
| GIC id | 121 | [`dma4`](dma4.md) | `GIC_SPI 89` |
| GIC id | 125 | [`aux`](aux.md) | `GIC_SPI 93` |
| GIC id | 129 | [`hvs`](hvs.md) | `GIC_SPI 97` |
| GIC id | 145 | [`gpio`](gpio.md) `BANK0` | `GIC_SPI 113` |
| GIC id | 146 | [`gpio`](gpio.md) `BANK1` | `GIC_SPI 114` |
| GIC id | 147 | [`gpio`](gpio.md) `BANK1_MIRROR` | `GIC_SPI 115` |
| GIC id | 148 | [`gpio`](gpio.md) `ANY` | `GIC_SPI 116` |
| GIC id | 149 | [`bsc`](bsc.md) | `GIC_SPI 117` |
| GIC id | 150 | [`spi0`](spi0.md) | `GIC_SPI 118` |
| GIC id | 153 | [`uart0`](uart0.md) | `GIC_SPI 121` |
| GIC id | 158 | [`emmc2`](emmc2.md) | `GIC_SPI 126` |
| GIC id | 158 | [`emmc`](emmc.md) | `GIC_SPI 126` |
| GIC id | 175 | [`pcie`](pcie.md) `INTA` | `GIC_SPI 143` |
| GIC id | 180 | [`pcie`](pcie.md) `MSI` | `GIC_SPI 148` |
| GIC id | 189 | [`genet`](genet.md) `INTRL2_0` | `GIC_SPI 157` |
| GIC id | 190 | [`genet`](genet.md) `INTRL2_1` | `GIC_SPI 158` |
| GIC id | 208 | [`xhci_otg`](xhci_otg.md) | `GIC_SPI 176` |
