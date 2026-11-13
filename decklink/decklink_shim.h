#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DlCapture DlCapture;

typedef void (*DlFrameCallback)(
    void* user,
    const uint8_t* data,
    uint32_t width,
    uint32_t height,
    uint32_t stride,
    int32_t fps_n,
    int32_t fps_d,
    int32_t has_signal
);

// Enumerate installed DeckLink devices.
// Calls `cb(user, utf8_display_name)` for each device in index order.
// Returns count of devices (>= 0), or 0 if Desktop Video is not installed.
int32_t dl_list_devices(void (*cb)(void* user, const char* name), void* user);

// Start capture on device at `index`.
// On failure, writes a null-terminated UTF-8 error message to `err_buf` and returns NULL.
DlCapture* dl_start(
    uint32_t index,
    DlFrameCallback cb,
    void* user,
    char* err_buf,
    uint32_t err_cap
);

// Stop capture, disable input, and release COM objects (idempotent).
void dl_stop(DlCapture* cap);

// Stop (if running) and destroy the DlCapture instance.
void dl_destroy(DlCapture* cap);

// Copy current human-readable status (UTF-8) into `buf` (up to `cap_len` bytes, null-terminated).
// Returns number of bytes written (excluding null terminator).
uint32_t dl_status(DlCapture* cap, char* buf, uint32_t cap_len);

#ifdef __cplusplus
}
#endif
