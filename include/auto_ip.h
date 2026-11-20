#ifndef AUTO_IP_H
#define AUTO_IP_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define AUTO_IP_ABI_VERSION 1
#define AUTO_IP_EVENT_SENT 0
#define AUTO_IP_EVENT_REPLY 1

#if defined(_WIN32) && !defined(AUTO_IP_STATIC)
#define AUTO_IP_API __declspec(dllimport)
#else
#define AUTO_IP_API
#endif

typedef struct auto_ip_config {
    const char *interface_name; /* NULL: pick the first usable interface */
    const char *source_mac;     /* NULL: use the interface MAC */
    const char *base_ip;
    const char *netmask;
    const char *gateway;
    uint16_t ttl;
    uint8_t index_offset;
    uint8_t reserved;
    uint32_t count;             /* 0: send forever */
    uint64_t interval_ms;
    uint64_t reply_timeout_ms;  /* 0: do not collect replies */
} auto_ip_config;

typedef struct auto_ip_reply {
    uint8_t mac[6];
    uint8_t status;             /* 0 applied, 1 unchanged, 2 save-failed, 3 rejected, 4 pending */
    uint8_t reason;
    uint16_t index;
    uint16_t ttl;
    uint32_t old_ip;            /* network byte order, as in struct in_addr.s_addr */
    uint32_t new_ip;            /* network byte order, as in struct in_addr.s_addr */
} auto_ip_reply;

typedef struct auto_ip_event {
    uint32_t kind;              /* AUTO_IP_EVENT_SENT or AUTO_IP_EVENT_REPLY */
    uint32_t sent;              /* valid for AUTO_IP_EVENT_SENT */
    auto_ip_reply reply;        /* valid for AUTO_IP_EVENT_REPLY */
} auto_ip_event;

/*
 * Called from the run worker thread. event is valid only for the duration of
 * the callback. The callback must not throw or unwind across the C ABI.
 */
typedef void (*auto_ip_event_callback)(const auto_ip_event *event, void *user_data);

typedef struct auto_ip_interface {
    const char *name;           /* owned by the interface list */
    const char *description;    /* owned by the interface list */
    uint8_t mac[6];
    uint8_t has_mac;
    uint8_t is_up;
    uint8_t is_loopback;
    uint8_t reserved;
} auto_ip_interface;

typedef struct auto_ip_interface_list auto_ip_interface_list;
typedef struct auto_ip_run auto_ip_run;
typedef struct auto_ip_summary auto_ip_summary;
typedef struct auto_ip_preview auto_ip_preview;

/* Stable ABI version and library version. The version string is static. */
AUTO_IP_API uint32_t auto_ip_abi_version(void);
AUTO_IP_API const char *auto_ip_version(void);

/* Free a string returned through an error out-parameter. */
AUTO_IP_API void auto_ip_string_free(char *value);

/* Fill cfg with defaults; call this before changing fields. */
AUTO_IP_API void auto_ip_config_init(auto_ip_config *cfg);

/*
 * Enumerate interfaces visible to the packet backend.
 * On success returns 0 and stores a list in out_list; free it with
 * auto_ip_interface_list_free. On Windows the Npcap driver must be installed.
 */
AUTO_IP_API int auto_ip_list_interfaces(auto_ip_interface_list **out_list, char **error);
AUTO_IP_API size_t auto_ip_interface_list_count(const auto_ip_interface_list *list);
AUTO_IP_API int auto_ip_interface_list_get(
    const auto_ip_interface_list *list,
    size_t index,
    auto_ip_interface *out);
AUTO_IP_API void auto_ip_interface_list_free(auto_ip_interface_list *list);

/*
 * Prepare a frame without sending it. This binds the interface and calculates
 * the frame and nonce. frame memory is owned by the preview.
 */
AUTO_IP_API int auto_ip_preview_build(
    const auto_ip_config *cfg,
    auto_ip_preview **out_preview,
    char **error);
AUTO_IP_API const char *auto_ip_preview_interface(const auto_ip_preview *preview);
AUTO_IP_API const char *auto_ip_preview_source_mac(const auto_ip_preview *preview);
AUTO_IP_API const uint8_t *auto_ip_preview_frame(const auto_ip_preview *preview);
AUTO_IP_API size_t auto_ip_preview_frame_len(const auto_ip_preview *preview);
AUTO_IP_API uint32_t auto_ip_preview_nonce(const auto_ip_preview *preview);
AUTO_IP_API void auto_ip_preview_free(auto_ip_preview *preview);

/*
 * Parse one received Ethernet frame. Returns 0 when the frame is a valid
 * AUTO_IP reply for nonce, -1 otherwise. Reply IP fields use network byte order.
 */
AUTO_IP_API int auto_ip_parse_reply(
    const uint8_t *frame,
    size_t len,
    uint32_t nonce,
    auto_ip_reply *out_reply);

/*
 * Start a run on a worker thread. The callback may be NULL.
 * cfg strings are copied before this function returns. user_data must stay
 * valid until the run finishes.
 * On success returns 0 and stores a run handle. Call auto_ip_run_wait exactly
 * once to join and obtain the summary, then auto_ip_run_free.
 */
AUTO_IP_API int auto_ip_run_start(
    const auto_ip_config *cfg,
    auto_ip_event_callback callback,
    void *user_data,
    auto_ip_run **out_run,
    char **error);

/*
 * Block until the run finishes. out_summary and error must not be NULL.
 * Do not call this from the event callback. Returns 0 on success; free the
 * summary with auto_ip_summary_free.
 */
AUTO_IP_API int auto_ip_run_wait(
    auto_ip_run *run,
    auto_ip_summary **out_summary,
    char **error);

/* Request cancellation; safe to call from another thread or the callback. */
AUTO_IP_API void auto_ip_run_stop(auto_ip_run *run);

/* Stop and join if necessary, then free the run handle. */
AUTO_IP_API void auto_ip_run_free(auto_ip_run *run);

/*
 * Convenience: start, wait and free in one call. Cancellation is only possible
 * with the start/stop/wait API.
 */
AUTO_IP_API int auto_ip_run_blocking(
    const auto_ip_config *cfg,
    auto_ip_event_callback callback,
    void *user_data,
    auto_ip_summary **out_summary,
    char **error);

/* Summary strings and reply arrays are owned by the summary. */
AUTO_IP_API const char *auto_ip_summary_interface(const auto_ip_summary *summary);
AUTO_IP_API const char *auto_ip_summary_source_mac(const auto_ip_summary *summary);
AUTO_IP_API uint32_t auto_ip_summary_sent(const auto_ip_summary *summary);
AUTO_IP_API size_t auto_ip_summary_reply_count(const auto_ip_summary *summary);
AUTO_IP_API size_t auto_ip_summary_board_count(const auto_ip_summary *summary);
AUTO_IP_API int auto_ip_summary_reply(
    const auto_ip_summary *summary,
    size_t index,
    auto_ip_reply *out_reply);
AUTO_IP_API int auto_ip_summary_board(
    const auto_ip_summary *summary,
    size_t index,
    auto_ip_reply *out_reply);
AUTO_IP_API void auto_ip_summary_free(auto_ip_summary *summary);

#ifdef __cplusplus
}
#endif

#endif
