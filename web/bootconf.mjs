// The EEPROM bootloader's settings (`bootconf.txt`), as a form.
//
// `defaults` are the values Raspberry Pi documents for the Pi 4 bootloader; a
// value read from the EEPROM image wins over them. Only a setting the person
// changed leaves the form as a `KEY=VALUE` line.

const BOOL = [["0", "0 – off"], ["1", "1 – on"]];

export const BOOT_DEVICES = [
  ["1", "SD card"],
  ["2", "Network (TFTP)"],
  ["3", "RPIBOOT (USB device)"],
  ["4", "USB mass storage"],
  ["5", "USB-C (BCM) mass storage"],
  ["6", "NVMe"],
  ["7", "HTTP"],
  ["e", "Stop"],
  ["f", "Restart"],
];

export const BOOT_ORDER_PRESETS = [
  ["SD card, then restart", ["1", "f"]],
  ["SD card, then USB, then restart", ["1", "4", "f"]],
  ["USB, then SD card, then restart", ["4", "1", "f"]],
  ["USB-C stick, then SD card", ["5", "1", "f"]],
  ["Network (TFTP), then restart", ["2", "f"]],
  ["HTTP, then restart", ["7", "f"]],
  ["SD card, then network", ["1", "2", "f"]],
];

const text = (key, label, help, extra = {}) => ({ key, label, help, type: "text", ...extra });
const int = (key, label, help, def, extra = {}) => ({ key, label, help, type: "int", default: def, ...extra });
const select = (key, label, help, options, def) => ({ key, label, help, type: "select", options, default: def });
const bool = (key, label, help, def) => select(key, label, help, BOOL, def);

export const GROUPS = [
  {
    title: "Serial and power",
    settings: [
      bool("BOOT_UART", "Bootloader serial console", "Print the bootloader's own log on the UART.", "0"),
      bool("WAKE_ON_GPIO", "Wake on GPIO", "Wake from halt when GPIO3 is pulled low.", "1"),
      bool("POWER_OFF_ON_HALT", "Power off on halt", "Cut power instead of halting.", "0"),
      int("PSU_MAX_CURRENT", "Power supply limit (mA)", "0 leaves the supply's limit unset.", "0"),
      bool("WAIT_FOR_POWER_BUTTON", "Wait for the power button", "Stay off until the button is pressed.", ""),
    ],
  },
  {
    title: "Retries and restarts",
    settings: [
      int("SD_BOOT_MAX_RETRIES", "SD card retries", "Tries before moving to the next boot mode; -1 never gives up.", "0"),
      int("NET_BOOT_MAX_RETRIES", "Network retries", "Tries before moving to the next boot mode; -1 never gives up.", "0"),
      int("USB_DEV_BOOT_MAX_RETRIES", "USB device retries", "Tries before moving to the next boot mode.", ""),
      int("MAX_RESTARTS", "Maximum restarts", "How many times RESTART loops before the bootloader stops.", ""),
      bool("REBOOT_ON_FATAL_ERROR", "Reboot on a fatal error", "", ""),
      int("PARTITION", "Partition", "Boot partition for A/B boots.", "0"),
      bool("PARTITION_WALK", "Walk the partitions", "Try the next partition when this one does not boot.", ""),
      bool("TRYBOOT_A_B_MODE", "Tryboot A/B mode", "", ""),
      int("BOOT_WATCHDOG_TIMEOUT", "Boot watchdog (s)", "", ""),
      int("BOOT_WATCHDOG_PARTITION", "Boot watchdog partition", "", ""),
    ],
  },
  {
    title: "Network boot",
    settings: [
      int("DHCP_TIMEOUT", "DHCP timeout (ms)", "", "45000"),
      int("DHCP_REQ_TIMEOUT", "DHCP request timeout (ms)", "", "4000"),
      int("TFTP_FILE_TIMEOUT", "TFTP file timeout (ms)", "", "30000"),
      text("TFTP_IP", "TFTP server", "An IP address, in place of the one DHCP names."),
      select("TFTP_PREFIX", "TFTP directory", "What names the directory the files are fetched from.", [
        ["0", "0 – the board's serial number"],
        ["1", "1 – TFTP_PREFIX_STR"],
        ["2", "2 – the MAC address"],
      ], "0"),
      text("TFTP_PREFIX_STR", "TFTP directory name", "Used when the TFTP directory is 1."),
      bool("USE_IPV6", "IPv6", "", "0"),
      bool("DHCP_OPTION97", "DHCP option 97", "Send the client identifier.", ""),
      text("CLIENT_IP", "Static client address", "Skips DHCP when set together with the next ones."),
      text("SUBNET", "Subnet mask", ""),
      text("GATEWAY", "Gateway", ""),
      text("DNS_IP", "DNS server", ""),
      text("MAC_ADDRESS", "MAC address", "aa:bb:cc:dd:ee:ff"),
      text("PXE_OPTION43", "PXE option 43", ""),
      text("NETCONSOLE", "Netconsole", ""),
    ],
  },
  {
    title: "HTTP boot and network install",
    settings: [
      text("HTTP_HOST", "HTTP server", "Host the HTTP boot fetches boot.img from."),
      int("HTTP_PORT", "HTTP port", "", "80"),
      text("HTTP_PATH", "HTTP path", "", { default: "net_install" }),
      text("HTTP_CACERT_HASH", "CA certificate hash", "SHA-256 of the certificate to trust."),
      bool("NET_INSTALL_ENABLED", "Network install", "Allow the Raspberry Pi Imager network install.", "1"),
      bool("NET_INSTALL_AT_POWER_ON", "Network install at power on", "", "0"),
      int("NET_INSTALL_KEYBOARD_WAIT", "Keyboard wait (ms)", "How long to wait for a keypress to start the install.", ""),
    ],
  },
  {
    title: "USB",
    settings: [
      int("USB_MSD_DISCOVER_TIMEOUT", "Discovery timeout (ms)", "", "20000"),
      int("USB_MSD_LUN_TIMEOUT", "LUN timeout (ms)", "", "2000"),
      int("USB_MSD_PWR_OFF_TIME", "Power-off time (ms)", "", "1000"),
      int("USB_MSD_STARTUP_DELAY", "Startup delay (ms)", "", "0"),
      int("USB_DEV_FILE_TIMEOUT", "File timeout (ms)", "", ""),
      text("USB_MSD_EXCLUDE_VID_PID", "Excluded devices", "Comma separated vid:pid pairs."),
      int("XHCI_DEBUG", "xHCI debug mask", "", "0"),
      bool("SD_OVERCURRENT_CHECK", "SD overcurrent check", "", ""),
    ],
  },
  {
    title: "Display",
    settings: [
      int("HDMI_DELAY", "HDMI delay (s)", "How long the bootloader waits for a monitor.", "5"),
      bool("DISABLE_HDMI", "Disable HDMI", "", "0"),
    ],
  },
  {
    title: "Updates and security",
    settings: [
      bool("ENABLE_SELF_UPDATE", "Self-update", "Update the EEPROM from a newer image on the boot medium.", "1"),
      bool("FREEZE_VERSION", "Freeze the version", "Refuse any update of this EEPROM.", "0"),
      bool("SIGNED_BOOT", "Signed boot", "Verify boot.img against the public key in the EEPROM.", "0"),
    ],
  },
];

export const KNOWN_KEYS = new Set(GROUPS.flatMap((g) => g.settings.map((s) => s.key)).concat("BOOT_ORDER"));

// A BOOT_ORDER value reads right to left: the last hex digit is tried first.
export function parseBootOrder(value) {
  const digits = String(value).trim().replace(/^0x/i, "").toLowerCase();
  if (!/^[0-9a-f]{1,8}$/.test(digits)) return null;
  return [...digits].reverse();
}

export function formatBootOrder(steps) {
  return "0x" + [...steps].reverse().join("");
}

// The image's own `[all]` settings, as a Map.
export function imageSettings(entries) {
  const values = new Map();
  for (const [section, key, value] of entries) {
    if (section === "all") values.set(key, value);
  }
  return values;
}

export function conditionalSections(entries) {
  return [...new Set(entries.map((e) => e[0]).filter((s) => s !== "all"))];
}

// `KEY=VALUE` for every setting whose value differs from what the image (or the
// documentation) has; the boot order only when `order` is set.
export function editedLines(values, base, order) {
  const lines = [];
  for (const group of GROUPS) {
    for (const s of group.settings) {
      const now = values.get(s.key);
      if (now === undefined || now === "") continue;
      if (now !== (base.get(s.key) ?? s.default ?? "")) lines.push(`${s.key}=${now}`);
    }
  }
  if (order) lines.push(`BOOT_ORDER=${formatBootOrder(order)}`);
  return lines;
}
