<!-- generated from specs/*.toml by `cargo run -- spec-docs --update` – do not edit -->

# Peripheral register specs

Generated from the TOML specs in [`specs/`](../../specs/); see [`specs/README.md`](../../specs/README.md) for the format and the rules.

| Block | Bus | Base | Size | Registers | Summary |
|---|---|---|---|---|---|
| [`armctrl`](armctrl.md) | vpu | `0x7E00B000` | `0x880` | 3 | ARM control block as the VPU sees it, below the mailboxes: where arm_loader releases the ARM |
| [`armlocal`](armlocal.md) | arm | `0xFF800000` | `0x100` | 2 | ARM local block: only the two registers the armstub writes |
| [`asb`](asb.md) | vpu | `0x7E00A000` | `0x1000` | 4 | AXI async slave bridges: the stop / acknowledge handshake before gating the V3D, ISP and H264 power domains |
| [`aux`](aux.md) | vpu | `0x7E215000` | `0x100` | 13 | AUX: mini-UART (UART1) and the SPI1 / SPI2 masters |
| [`avs`](avs.md) | vpu | `0x7D5D2000` | `0xF00` | 10 | AVS monitor: on-die temperature sensor and the ring-oscillator / rail voltage monitors |
| [`bcm54213pe`](bcm54213pe.md) | mdio | `0x01` | `0x20` | 23 | BCM54213PE gigabit Ethernet PHY on GENET's MDIO bus |
| [`bootbox`](bootbox.md) | vpu | `0x7EE00000` | `0x4000` | 9 | Boot-info handoff doorbells, and the VPU interrupt window start4's exception-12 handler reads |
| [`bsc`](bsc.md) | vpu | `0x7E205000` | `0x20` | 8 | BSC (I²C master): instance 0 with nothing attached, and the instance the board PMICs and GPIO expander sit on |
| [`clkmon`](clkmon.md) | vpu | `0x7D5D0000` | `0x10000` | 5 | VPU clock block (PLLs and frequency monitors) below the 0x7E window |
| [`cm`](cm.md) | vpu | `0x7E101000` | `0x2000` | 4 | Clock manager, with the A2W PLL control in the same window |
| [`corectl`](corectl.md) | vpu | `0x7E002000` | `0x1000` | 5 | VPU core control: per-core boot handshake and interrupt controller |
| [`dma`](dma.md) | vpu | `0x7E007000` | `0x1000` | 11 | Legacy DMA controller: 15 channels 0x100 apart plus the controller-wide interrupt status and enable words |
| [`dma4`](dma4.md) | vpu | `0x7E007B00` | `0x100` | 3 | DMA4 ('dma40') channel: the 40-bit DMA engine the bootloader and start4 use |
| [`dma_vpu`](dma_vpu.md) | vpu | `0x7EE04100` | `0x1000` | 9 | The DMA controller start4's dmalib drives: 16 channel slots, channel 15 at 0x7EE05000 |
| [`dwc2`](dwc2.md) | vpu | `0x7E980000` | `0x10000` | 17 | DesignWare USB 2.0 OTG controller (the USB-C port): the reset start4 runs when USB power comes on, and a host port with nothing plugged in |
| [`emmc`](emmc.md) | vpu | `0x7E300000` | `0x100` | 20 | The legacy EMMC controller (Arasan SDHCI): the Pi 4's WiFi SDIO host, and the host 2020-era bootcode reads the SD card through |
| [`emmc2`](emmc2.md) | vpu | `0x7E340000` | `0x1000` | 22 | EMMC2: the SD host controller (Arasan SDHCI 3.00) the bootloader, start4 and Linux boot from |
| [`fxl6408`](fxl6408.md) | i2c | `0x43` | `0x100` | 10 | FXL6408 GPIO expander: the board's 'external' GPIOs 128..135 |
| [`genet`](genet.md) | vpu | `0x7D580000` | `0x10000` | 56 | GENET v5 Ethernet MAC with its UniMAC MDIO controller (the PHY is `bcm54213pe`) |
| [`gicc`](gicc.md) | arm | `0xFF842000` | `0x2000` | 15 | GIC-400 CPU interface, banked per CPU and per security state |
| [`gicd`](gicd.md) | arm | `0xFF841000` | `0x1000` | 16 | GIC-400 distributor |
| [`gich`](gich.md) | arm | `0xFF844000` | `0x2000` | 10 | GIC-400 virtual interface control, banked per CPU |
| [`gicv`](gicv.md) | arm | `0xFF846000` | `0x2000` | 0 | GIC-400 virtual CPU interface |
| [`hd`](hd.md) | vpu | `0x7E808000` | `0x100` | 2 | Control block at 0x7E80_8000: the power request and acknowledge start4 waits on before it resets the DWC2 USB controller |
| [`hdmi`](hdmi.md) | vpu | `0x7EF00700` | `0x300` | 4 | HDMI controller core registers (the "hdmi" window of each connector), with no monitor attached: the packet-RAM handshake, the FIFO recenter and the hotplug state |
| [`hdmi_auto_i2c`](hdmi_auto_i2c.md) | vpu | `0x7EF00B00` | `0x300` | 5 | HDMI auto-i2c sequencers (the second reg window of each DDC master's node): channels that write a list of values into their connector's DDC I²C master and report when the transfer it starts has finished |
| [`hdmi_ddc`](hdmi_ddc.md) | vpu | `0x7EF04500` | `0x100` | 8 | HDMI DDC I²C masters (brcm,bcm2711-hdmi-i2c), one per connector: the bus a monitor's EDID EEPROM sits on |
| [`hvs`](hvs.md) | vpu | `0x7E400000` | `0x1000` | 7 | HVS (Hardware Video Scaler): identification, the per-channel frame-swap words, and end of frame |
| [`mbox`](mbox.md) | vpu | `0x7E00B880` | `0x140` | 12 | ARM <-> VideoCore mailboxes: two views of the same pair of FIFOs, and the interrupt block between them |
| [`mcsync`](mcsync.md) | vpu | `0x7E000000` | `0x1000` | 4 | Doorbells / semaphores between the two VPU cores |
| [`otp`](otp.md) | vpu | `0x7E20F000` | `0x1000` | 7 | Always-on config / OTP engine: the fuse array, one row at a time – reads and programming |
| [`pcie`](pcie.md) | vpu | `0x7D500000` | `0x9310` | 36 | PCIe root complex (pcie-brcmstb), with the VL805 xHCI controller behind it |
| [`pm`](pm.md) | vpu | `0x7E100000` | `0x1000` | 6 | Power management: reset control, reset status, watchdog, power-domain registers |
| [`pmic_1d`](pmic_1d.md) | i2c | `0x1D` | `0x100` | 6 | Board PMIC at 0x1D on every Pi 4-family board but the 4B rev 1.5 (start4 descriptor type 0x81) |
| [`pmic_core`](pmic_core.md) | i2c | `0x1E` | `0x100` | 2 | Board PMIC owning the SoC core rail (start4 descriptor type 0x82) |
| [`pmic_rails`](pmic_rails.md) | i2c | `0x1B` | `0x100` | 5 | Board PMIC owning the SDRAM and I/O rails (start4 descriptor type 0x83) |
| [`pvt`](pvt.md) | vpu | `0x7D5D8000` | `0x480` | 5 | Per-channel PVT (process / voltage / temperature) monitors |
| [`rng`](rng.md) | vpu | `0x7E104000` | `0x28` | 10 | Hardware RNG (RNG200): generator control, warm-up counter, interrupt and FIFO |
| [`sdc`](sdc.md) | vpu | `0x7E001000` | `0x1000` | 4 | Legacy SDRAM-controller interface: DRAM timing table, sub-controller ready bits, LPDDR4 mode-register port |
| [`sdramc`](sdramc.md) | vpu | `0x7DC00000` | `0x40000` | 13 | LPDDR4 controller and PHY, below the 0x7E window |
| [`spi0`](spi0.md) | vpu | `0x7E204000` | `0x18` | 6 | SPI0 master, with the serial-NOR flash the EEPROM bootloader was loaded from |
| [`systimer`](systimer.md) | vpu | `0x7E003000` | `0x1000` | 4 | System timer: 64-bit free-running 1 MHz counter with four compare channels |
| [`uart0`](uart0.md) | vpu | `0x7E201000` | `0x1000` | 11 | PL011 UART0: the firmware's debug console and Linux's ttyAMA0 |
| [`vce`](vce.md) | vpu | `0x7F100000` | `0x21000` | 3 | VCE vector/codec engine: data memory, program memory and register file |
| [`vce_ctrl`](vce_ctrl.md) | vpu | `0x7F140000` | `0x1000` | 6 | VCE control block: status, launch, interrupt clear and endcode enables |
| [`vl805`](vl805.md) | pci | `0x00000000` | `0x1000` | 26 | VIA VL805 xHCI controller (1106:3483): its PCI configuration space |
| [`xhci`](xhci.md) | pci | `0x00000000` | `0x1000` | 39 | xHCI register block behind the VL805's BAR0: capability, operational, runtime and doorbell registers |
