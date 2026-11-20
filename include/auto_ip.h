#ifndef AUTO_IP_H
#define AUTO_IP_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define AUTO_IP_ABI_VERSION 1

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
typedef struct auto_ip_result auto_ip_result;

/* Stable ABI version and library version. The version string is static. */
AUTO_IP_API uint32_t auto_ip_abi_version(void);
AUTO_IP_API const char *auto_ip_version(void);

/* Free a string returned through an error out-parameter. */
AUTO_IP_API void auto_ip_string_free(char *value);

/* Fill cfg with the CLI defaults; call this before changing fields. */
AUTO_IP_API void auto_ip_config_init(auto_ip_config *cfg);

/* Same as `auto-ip --list`. On success free the list. */
AUTO_IP_API int auto_ip_list_interfaces(auto_ip_interface_list **out_list, char **error);
AUTO_IP_API size_t auto_ip_interface_list_count(const auto_ip_interface_list *list);
AUTO_IP_API int auto_ip_interface_list_get(
    const auto_ip_interface_list *list,
    size_t index,
    auto_ip_interface *out);
AUTO_IP_API void auto_ip_interface_list_free(auto_ip_interface_list *list);

/*
 * Send frames and collect replies, blocking until done. This is the C
 * equivalent of running the CLI with the fields from cfg.
 *
 * On success returns 0 and stores a result in out_result. On failure returns
 * -1, leaves out_result NULL, and optionally stores an allocated error string.
 * Free the result with auto_ip_result_free and the error with auto_ip_string_free.
 */
AUTO_IP_API int auto_ip_run(
    const auto_ip_config *cfg,
    auto_ip_result **out_result,
    char **error);

/* Result strings and reply arrays are owned by the result. */
AUTO_IP_API const char *auto_ip_result_interface(const auto_ip_result *result);
AUTO_IP_API const char *auto_ip_result_source_mac(const auto_ip_result *result);
AUTO_IP_API uint32_t auto_ip_result_sent(const auto_ip_result *result);
AUTO_IP_API size_t auto_ip_result_reply_count(const auto_ip_result *result);
AUTO_IP_API size_t auto_ip_result_board_count(const auto_ip_result *result);
AUTO_IP_API int auto_ip_result_reply(
    const auto_ip_result *result,
    size_t index,
    auto_ip_reply *out_reply);
AUTO_IP_API int auto_ip_result_board(
    const auto_ip_result *result,
    size_t index,
    auto_ip_reply *out_reply);
AUTO_IP_API void auto_ip_result_free(auto_ip_result *result);

#ifdef __cplusplus
}
#endif

#endif
