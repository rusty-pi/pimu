/*
 * rpi-virt-fw: the VideoCore firmware model as a C library.
 *
 * Mirrors src/capi.rs by hand — there is no bindgen in the build, so that the
 * ABI is a reviewed artefact. Change both together.
 *
 * Threading: nothing in here locks. Serialise every call on one rvf_vc
 * yourself. Callbacks run on the calling thread, from inside the call that
 * triggered them.
 *
 * Addresses are VideoCore bus addresses throughout (0x7E00B880 for the ARM
 * mailbox), which is what both the firmware and the ARM's 0xFExxxxxx window
 * decode to.
 */
#ifndef RPI_VIRT_FW_H
#define RPI_VIRT_FW_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct rvf_vc rvf_vc;

/* Callbacks into the host. Any pointer may be NULL. */
typedef struct rvf_host_ops {
    void *opaque;
    /* A VPU access to a window registered with rvf_vc_add_foreign(). */
    uint32_t (*mmio_read)(void *opaque, uint32_t addr, uint32_t size);
    void (*mmio_write)(void *opaque, uint32_t addr, uint32_t size,
                       uint32_t value);
    /* Bytes the firmware wrote to the console UART the model owns. Not
     * called while UART0 is a foreign window. */
    void (*console)(void *opaque, const uint8_t *buf, size_t len);
    /* A diagnostic line, NUL-terminated, no trailing newline. */
    void (*log)(void *opaque, const char *msg);
    /* The firmware released the ARM cores. Called once. */
    void (*arm_release)(void *opaque);
} rvf_host_ops;

/* rvf_vc_run() statuses. */
#define RVF_RUN_RUNNING 0 /* call again */
#define RVF_RUN_STOPPED 1 /* the VPU stopped for good; reason went to log() */
#define RVF_RUN_RESET   2 /* the firmware asked the power manager for a reset */

/*
 * Create a VideoCore over guest RAM.
 *
 * `ram` must stay valid for the object's whole life; the model only copies
 * bytes in and out of it, and other bus masters may write it concurrently.
 * At most 1 GiB is used (the VC4 address fold). `eeprom` (pieeprom.bin) and
 * `sd` (a raw card image, may be NULL) are copied. `ops` is copied by value.
 * Returns NULL and fills `err` (NUL-terminated, `err_len` bytes) on failure.
 */
rvf_vc *rvf_vc_new(uint8_t *ram, size_t ram_len,
                   const uint8_t *eeprom, size_t eeprom_len,
                   const uint8_t *sd, size_t sd_len,
                   const rvf_host_ops *ops,
                   char *err, size_t err_len);
void rvf_vc_free(rvf_vc *vc);

/*
 * Forward VPU accesses in [lo, hi) to ops->mmio_read/mmio_write instead of
 * the model's own peripheral. Only before the first rvf_vc_run(); returns 0,
 * or -1 once the model has started.
 */
int rvf_vc_add_foreign(rvf_vc *vc, uint32_t lo, uint32_t hi);

/* Run up to `max_steps` VPU instructions. Returns an RVF_RUN_* status. */
int rvf_vc_run(rvf_vc *vc, uint64_t max_steps);

/* Model time in microseconds: the VPU's system-timer counter. */
uint64_t rvf_vc_now_us(const rvf_vc *vc);
/* Instructions retired by core 0. */
uint64_t rvf_vc_retired(const rvf_vc *vc);
/* Whether ops->arm_release has fired. */
int rvf_vc_arm_released(const rvf_vc *vc);
/* Level of the ARM's mailbox interrupt (GIC SPI 33 on BCM2711). */
int rvf_vc_arm_irq(const rvf_vc *vc);

/*
 * An ARM-side access to one of the model's peripherals. `size` is 1, 2 or 4.
 * Unmapped or malformed accesses read as 0 and writes are dropped.
 */
uint32_t rvf_vc_mmio_read(rvf_vc *vc, uint32_t addr, uint32_t size);
void rvf_vc_mmio_write(rvf_vc *vc, uint32_t addr, uint32_t size,
                       uint32_t value);

#ifdef __cplusplus
}
#endif

#endif /* RPI_VIRT_FW_H */
